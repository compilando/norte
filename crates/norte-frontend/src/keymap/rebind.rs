//! The gate between a chord the user just pressed and a `keymap.toml` that
//! will not load (K3c c2).
//!
//! `norte-config` writes `keymap.toml` (c1), but it sits BELOW the keymap
//! grammar: it cannot parse a chord, look a command up in the catalogue, or
//! apply any of ADR 0006's whole-map rules. Its own contract says exactly that
//! and names this module as the caller's gate. So this is the only thing
//! standing between a captured chord and a user layer that fails to load —
//! and the consequence of one is out of all proportion to the typo that caused
//! it. The TUI's `reload_config` applies a reload ALL or NOTHING: a keymap that
//! does not build leaves every previous value in place (`msg-config-not-applied`
//! and nothing else), so the file on disk and the keymap in memory drift apart
//! and stay that way until the next start-up, which then fails too. The user
//! gets a message about their config and a key that does nothing. **A rebind
//! this module refuses must never reach disk.**
//!
//! Two functions, and they are not alternatives:
//!
//! - [`rebind_check`] is the human answer, read off the effective map a
//!   frontend already holds: free, replaces X, or refused and why. Cheap
//!   enough to run on every captured chord, so the editor shows the verdict
//!   BEFORE asking for a confirmation.
//! - [`rebind_dry_run`] is the door. It applies the binding to the layer it
//!   will actually be written to, the way the writer applies it, re-runs the
//!   REAL loader over the reassembled stack, and then asks the built map who
//!   won — so the answer is [`Effective::build_for`]'s own verdict instead of a
//!   second implementation of its rules, and it hands back the very strings
//!   that passed for the writer to use unchanged. A rule added to the loader
//!   tomorrow is enforced here today, by construction.
//!
//! The editor calls both: `rebind_check` while capturing, `rebind_dry_run` on
//! confirm. On whether a sequence may be bound they agree, with one documented
//! exception (see [`rebind_check`]'s note on [`Rebind::Sacred`]); the door then
//! answers two further questions the classifier structurally cannot — whether
//! the COMMAND exists, and whether the binding would ever fire.
//!
//! Both are pure and both live in the engine, because the TUI and the GUI must
//! not each decide what a collision is.

use norte_config::{KeymapList, Layer};

use super::chord::Chord;
use super::effective::{Availability, Effective, Lookup, render_seq, sacred_chords};
use super::layer::{KeymapFile, RawBinding, Screen};
use super::{KeymapError, Mods, paint_chord, parse_chord};

/// What binding `seq` right now would collide with — or why it cannot be
/// bound at all.
///
/// Only [`Self::Free`] and [`Self::Replaces`] may be written; every other
/// variant is a LOAD error waiting to happen, which is why
/// [`Self::is_refusal`] exists rather than each frontend re-deriving the list.
///
/// Every CHORD in here is painted ([`paint_chord`]) and every COMMAND is raw.
/// That asymmetry is deliberate and it rests on an invariant: `parse_chord`
/// accepts any lone codepoint, so a `./.norte` layer — untrusted, it arrives
/// with a cloned repository — can bind `RLO` or `ZWSP`, while a `run` that
/// survived [`Effective::build_for`] is necessarily a name from the caller's
/// command set, a name from the shared catalogue, or `lua:` plus
/// `[a-z0-9._-]{1,64}`. The day a command name comes from somewhere less
/// constrained — a plugin, say — the commands need painting too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rebind {
    /// Nothing is in the way.
    Free,
    /// The exact sequence already runs something. Taking it over is what a
    /// `prepend_keymap` write is for ([`KeymapList`]) — but whether it would
    /// actually FIRE afterwards is not a question this verdict answers: it is
    /// read off the merged map, which no longer knows which layer anything came
    /// from. [`rebind_dry_run`] answers it, and refuses with
    /// [`RebindError::Shadowed`] when the answer is no.
    Replaces {
        /// What it runs today.
        command: String,
        /// Whether this build can run THAT — so the editor can say "replaces
        /// `pane.pack`, which is not built yet". With K2b's four imported
        /// presets naming about thirty commands norte has not written, this is
        /// the common case, not an exotic one: a Total Commander user
        /// reclaiming `Alt+F5` is reclaiming a key that does nothing today,
        /// and being told so is the difference between a confident rebind and
        /// a puzzled one.
        avail: Availability,
    },
    /// `seq` is a strict prefix of an existing sequence, or extends one —
    /// ADR 0006's prefix-free rule. Without timeouts the resolution has to be
    /// deterministic, so this is a LOAD error and the editor must refuse
    /// before writing rather than produce a file that will not load.
    PrefixClash {
        /// The sequence in the way, PAINTED (`g g`) — ready to show.
        with: String,
        /// What that sequence runs.
        command: String,
    },
    /// The first chord is reserved by the specification (§12: `Tab` switches
    /// panes, in Browse). A preset that imitates another program documents the
    /// difference; it does not take the key, and neither does the user's
    /// editor.
    Sacred {
        /// The command the key is reserved for (`pane.switch`).
        reserved_for: &'static str,
    },
    /// The first chord is a digit 1-9 and the active preset enables numeric
    /// counts (`5j`, ADR 0044): the key cannot be a count and a binding at
    /// once. `0` is exempt — a count never starts with zero — so it is not
    /// refused.
    DigitWithCounts,
    /// No chords at all. A UI that can confirm an empty capture would write
    /// `on = []`, which is a load error like any other.
    Empty,
    /// `esc` inside a multi-chord sequence: `Esc` always cancels a pending
    /// prefix, so such a binding is unreachable and the loader refuses it. As
    /// a LONE binding `esc` is fine, and this variant does not fire for it.
    EscInSequence,
    /// The chord has no written form the loader reads back as the same chord —
    /// so it cannot be persisted at all. Today the only way to build one is
    /// out of range of the TOML grammar ([`super::KeyCode::F`] takes any
    /// `u8`, while `parse_chord` accepts `f1`..`f12`); both bundled frontends
    /// clamp before they get here, which makes this a belt rather than a live
    /// bug. It is a cheap one, and the alternative is [`Rebind::Free`] on a
    /// chord that reverts the user's whole keymap the moment it is written.
    Unwritable {
        /// The offending chord as it would have been written, PAINTED.
        chord: String,
    },
}

impl Rebind {
    /// Whether this verdict forbids the write. `true` for everything except
    /// [`Self::Free`] and [`Self::Replaces`].
    ///
    /// The editor asks this instead of matching the variants itself: a variant
    /// added here (because the loader grew a rule) must refuse by DEFAULT in
    /// both frontends, not be silently treated as permission until someone
    /// remembers to extend two `match`es.
    ///
    /// ```
    /// use norte_frontend::keymap::Rebind;
    ///
    /// assert!(!Rebind::Free.is_refusal());
    /// assert!(Rebind::DigitWithCounts.is_refusal());
    /// ```
    #[must_use]
    pub fn is_refusal(&self) -> bool {
        !matches!(self, Self::Free | Self::Replaces { .. })
    }
}

/// What `seq` would collide with in `eff`, if the user bound it right now.
///
/// The screen and the count policy come from the map itself
/// ([`Effective::screen`], [`Effective::counts`]) rather than from parameters:
/// both are properties of the map that was built, and a caller that could pass
/// a different screen could be told `Tab` is free.
///
/// A chord bound only in ANOTHER screen is NOT a collision: `eff` is one
/// screen's merged map, and that is the whole question — the viewer's `q` has
/// nothing to say about the browser's.
///
/// Checks run in the loader's own order — per-binding shape, then prefix-free,
/// digits, sacred — so the verdict names the same defect
/// [`Effective::build_for`] would report first, rather than a different true
/// thing about the same sequence.
///
/// # It is one case STRICTER than the loader
///
/// [`Rebind::Sacred`] fires for any sequence whose first chord is reserved,
/// including the one shape the loader accepts: `tab` alone, bound to
/// `pane.switch`. That binding already exists in every preset, so refusing to
/// write it again costs nothing, and it lets the rule be stated without the
/// command — "a sacred key is not the editor's to hand out". The asymmetry is
/// deliberate and pinned by a test; everywhere else this function and
/// [`rebind_dry_run`] agree.
///
/// # What it does NOT answer
///
/// - Whether `command` exists. This function never sees it; the catalogue
///   check belongs to the loader, and [`rebind_dry_run`] runs it.
/// - Whether the new binding will actually WIN. `Effective` is the merged map
///   and no longer knows which layer each binding came from, so it cannot see
///   a `./.norte` project layer outranking the user's, nor a differently
///   SPELLED twin of the same chord sitting earlier in the very list the write
///   goes into. There are two independent twin pairs: modifier ORDER
///   (`alt+ctrl+p` for what `Display` writes as `ctrl+alt+p`) and the per-OS
///   ALIAS (`mod+p` for `ctrl+p` — `mod` resolves to ONE physical modifier,
///   this process's [`mod_key`](super::mod_key), so the pair holds under
///   [`ModKey::Ctrl`](super::ModKey::Ctrl) and is `cmd+p` under the Cmd
///   policy). Each makes a write that loads and never fires — the silent
///   failure this whole module exists to stop — so answering it is
///   [`rebind_dry_run`]'s job, which models the write and then asks the built
///   map who won.
///
/// ```
/// use norte_frontend::keymap::{
///     Availability, Effective, Rebind, Screen, parse_chord, parse_keymap, rebind_check,
/// };
///
/// let src = r#"
/// [pane]
/// keymap = [
///     { on = ["f5"], run = "pane.copy" },
///     { on = ["g", "g"], run = "cursor.top" },
/// ]
/// "#;
/// let preset = parse_keymap(src).unwrap();
/// let eff = Effective::build_for(&preset, &[], &["pane.copy", "cursor.top"], Screen::Browse)
///     .unwrap();
/// let chord = |s: &str| parse_chord(s).unwrap();
///
/// assert_eq!(rebind_check(&eff, &[chord("ctrl+j")]), Rebind::Free);
/// assert_eq!(
///     rebind_check(&eff, &[chord("f5")]),
///     Rebind::Replaces { command: "pane.copy".to_owned(), avail: Availability::Here },
/// );
/// // `g` alone would swallow `g g`: a load error, refused before writing.
/// assert!(rebind_check(&eff, &[chord("g")]).is_refusal());
/// // §12: Tab is not for sale.
/// assert!(rebind_check(&eff, &[chord("tab")]).is_refusal());
/// ```
#[must_use]
pub fn rebind_check(eff: &Effective, seq: &[Chord]) -> Rebind {
    // Per-chord shape first, exactly as `check_binding` does it: a sequence
    // that cannot be written has no collisions worth reporting.
    for c in seq {
        if !round_trips(*c) {
            return Rebind::Unwritable {
                chord: paint_chord(&c.to_string()),
            };
        }
    }
    let Some(first) = seq.first() else {
        return Rebind::Empty;
    };
    if seq.len() > 1 && seq.iter().any(|c| c.is_bare_esc()) {
        return Rebind::EscInSequence;
    }
    // Then the whole-map rules, in `build_for`'s order: prefix-free, digits,
    // sacred. The order is not cosmetic — `tab j` against a preset that binds
    // `tab` (all seven do) breaks BOTH the prefix rule and the sacred one, and
    // the loader reports the prefix. A gate that answered `Sacred` there would
    // send the reader looking for a rule that is not the one the file would
    // trip on.
    for b in eff.raw_bindings() {
        let (short, long) = if b.seq.len() < seq.len() {
            (&b.seq[..], seq)
        } else {
            (seq, &b.seq[..])
        };
        if short.len() < long.len() && long[..short.len()] == *short {
            return Rebind::PrefixClash {
                with: paint_chord(&render_seq(&b.seq)),
                command: b.run.clone(),
            };
        }
    }
    if eff.counts() && super::resolve::digit_of(*first).is_some_and(|d| d != 0) {
        return Rebind::DigitWithCounts;
    }
    for (code, reserved_for) in sacred_chords(eff.screen()) {
        if *first == Chord::new(Mods::default(), *code) {
            return Rebind::Sacred { reserved_for };
        }
    }
    // Last, because it is not a refusal: an exact match and a prefix clash are
    // mutually exclusive anyway (the map is already prefix-free, so a sequence
    // equal to one binding cannot be a prefix of another), and the scan runs in
    // precedence order — the order the resolver itself uses.
    for b in eff.raw_bindings() {
        if b.seq == seq {
            return Rebind::Replaces {
                command: b.run.clone(),
                avail: b.avail,
            };
        }
    }
    Rebind::Free
}

/// Everything [`norte_config::persist_keymap_bind`] needs, and nothing the
/// caller has to derive again.
///
/// It exists so that the strings that were VALIDATED are the strings that get
/// written. A caller that took only a yes/no from [`rebind_dry_run`] and then
/// rendered the chords itself would have re-opened the gap the dry run closed:
/// the loader agreed to one spelling and the file received another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebindWrite {
    /// The `[section]` to write into — this screen's own context
    /// ([`Screen::section`]), never `global`. It is [`RebindSources::screen`]'s
    /// section and nothing checks that against the map [`rebind_check`]
    /// answered about: give the door a different screen and the binding is
    /// written, legally, into a context the editor never looked at.
    pub section: &'static str,
    /// Always [`KeymapList::Prepend`], and it is not a detail: the merge order
    /// is layer prepends → preset → layer appends and the FIRST binding of a
    /// sequence wins, so an `append_keymap` entry for a key the preset already
    /// binds in the same context parses, loads, validates — and never fires,
    /// while the editor reports success. Carried in the struct rather than
    /// left to the caller for that reason alone.
    pub list: KeymapList,
    /// The chords as the validated layer spelled them. Hand THESE to the
    /// writer; do not re-render the sequence. Usually [`Chord`]'s `Display`,
    /// but the spelling ALREADY IN THE FILE when the target list carries a twin
    /// of this sequence under a different one (`mod+p` for `ctrl+p`) — the
    /// writer matches byte-exactly, so only these strings land on the entry
    /// that actually wins.
    pub chords: Vec<String>,
    /// The command, unchanged — carried so the whole writer call reads off one
    /// value.
    pub command: String,
}

/// Everything the loader would read after the write, SPLIT at the layer the
/// write lands in.
///
/// The split is the whole point and it is not bookkeeping: a keymap layer's
/// precedence decides who wins, so a model that appends the prospective
/// binding at the top of the stack answers a question nobody asked. The write
/// goes into ONE layer — the user's — and a `./.norte` project layer still
/// outranks it afterwards.
///
/// # Do not cut it by hand: [`Self::split_at`]
///
/// The cut is NOT derivable from
/// [`FrontendConfig::keymap_layers`](crate::config::FrontendConfig::keymap_layers)
/// alone. That is a bare `Vec<KeymapFile>` with one entry per layer dir that
/// HAS a `keymap.toml`, so a dir without one leaves no gap and the position of
/// an entry says nothing about which layer it is. Both ways of guessing are
/// wrong, and wrong silently:
///
/// - taking the last layer as `target` hands over the PROJECT layer when there
///   is one. The write is then modelled ABOVE the layer that actually outranks
///   it, [`RebindError::Shadowed`] — the whole reason for the split — stops
///   firing, and the door approves a write the real user layer cannot make
///   stick;
/// - taking the last non-project layer hands over the SYSTEM layer when the
///   user has no file yet, which is the first rebind of every new install. The
///   model then puts the write into a file nothing ever writes: a broken
///   system entry looks repairable, and the spelling handed to the writer is
///   the system file's rather than the new user layer's.
///
/// [`KeymapFile::is_project`] identifies `above`, and nothing on a
/// `KeymapFile` distinguishes System from User — which is the half that
/// matters. [`Self::split_at`] takes the kinds
/// ([`FrontendConfig::keymap_layer_kinds`](crate::config::FrontendConfig::keymap_layer_kinds))
/// and makes the cut once, so no caller has to.
///
/// The target must NOT also appear in `below` or `above`. A duplicate is not
/// harmless: `check_binding` runs per raw binding BEFORE the dedup, so the
/// stale copy of an entry this rebind repairs would still fail the load.
///
/// `preset` and `known_commands` are what the frontend already passes to
/// [`Effective::build_for`] for `screen`.
#[derive(Debug, Clone, Copy)]
pub struct RebindSources<'a> {
    /// The active preset.
    pub preset: &'a KeymapFile,
    /// Layers of LOWER precedence than the one being written (the system
    /// layer), in ascending precedence — as [`Effective::build_for`] takes them.
    pub below: &'a [KeymapFile],
    /// The layer the write lands in, as loaded. [`KeymapFile::default`] when
    /// there is no file yet.
    ///
    /// **Never a PROJECT layer**, and that is a precondition rather than
    /// advice: `./.norte` is not the editor's to write, and a project layer
    /// here has its `lua:` bindings discarded on merge — so the model would
    /// disagree with the file about a binding that was never written, and a
    /// `lua:` command would come back as [`RebindError::Shadowed`] with an
    /// EMPTY `by` (nothing runs the key at all), which is not a sentence any
    /// editor can show. [`rebind_dry_run`] states the precondition with a
    /// `debug_assert!`; in release it is the caller's error, not an
    /// assumption. [`Self::split_at`] cannot violate it.
    pub target: &'a KeymapFile,
    /// Layers of HIGHER precedence (the project layer), ascending. These still
    /// outrank the write, which is why they are modelled instead of assumed
    /// away.
    pub above: &'a [KeymapFile],
    /// The commands this frontend implements for `screen`. Screen-dependent in
    /// practice — a frontend adds its `dialog.*` set for [`Screen::Dialog`] —
    /// so it travels next to the screen rather than being remembered
    /// separately.
    pub known_commands: &'a [&'a str],
    /// The screen being edited. Take it from
    /// [`Effective::screen`] of the map [`rebind_check`] was asked about; the
    /// two must be the same screen or the two answers are about different maps.
    pub screen: Screen,
}

impl<'a> RebindSources<'a> {
    /// Cut a frontend's loaded keymap layers at the one a rebind is written
    /// to — the USER layer — using the kinds that came with them.
    ///
    /// `kinds` and `layers` are
    /// [`FrontendConfig::keymap_layer_kinds`](crate::config::FrontendConfig::keymap_layer_kinds)
    /// and
    /// [`FrontendConfig::keymap_layers`](crate::config::FrontendConfig::keymap_layers):
    /// parallel, index by index, in ascending precedence. Everything of lower
    /// precedence than the user layer becomes `below`, the user layer itself
    /// becomes `target` — [`KeymapFile::default`] when the user has no
    /// `keymap.toml` yet, which is why the result OWNS it and hands out
    /// [`RebindSources`] by reference ([`RebindSplit::sources`]) — and
    /// everything above becomes `above`.
    ///
    /// This exists so the cut is made once, in the place that can be tested,
    /// rather than at each frontend's editor: [`RebindSources`]'s own
    /// documentation lists the two ways of guessing it and what each one
    /// silently breaks.
    ///
    /// The accepted order is a leading run of [`Layer::System`], then at most
    /// one [`Layer::User`], then at most one [`Layer::Profile`]; the write
    /// target is the LAST of those two that is present, and everything left
    /// above the write must be [`Layer::Project`]. Anything else lands in
    /// `above`, which is the FAIL-CLOSED side: an unexpected order makes the
    /// door refuse writes it cannot model, never approve one it did not. The
    /// two vectors should be the same length (a `debug_assert!` says so); a
    /// shorter `kinds` also degrades into `above`.
    ///
    /// The profile step (spec 2026-08-26, D10) is why the target is the last
    /// and not the first: an active profile with its own `keymap.toml` sits
    /// above the user's, so writing into the user's would leave the rebind
    /// shadowed — visibly saved, and doing nothing.
    ///
    /// ```
    /// use norte_config::Layer;
    /// use norte_frontend::keymap::{
    ///     RebindSources, Screen, parse_chord, parse_keymap, parse_keymap_layer, rebind_dry_run,
    /// };
    ///
    /// let preset = parse_keymap("[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n")
    ///     .unwrap();
    /// // A new install: only the system dir ships a `keymap.toml`.
    /// let system =
    ///     parse_keymap_layer("[pane]\nprepend_keymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n")
    ///         .unwrap();
    /// let layers = [system];
    /// let kinds = [Layer::System];
    /// let known = ["pane.copy", "pane.move"];
    /// let split = RebindSources::split_at(&preset, &kinds, &layers, &known, Screen::Browse);
    /// // The write goes into the user's own (still absent) layer, above the system one.
    /// let w = rebind_dry_run(&split.sources(), &[parse_chord("f5").unwrap()], "pane.move").unwrap();
    /// assert_eq!((w.section, &w.chords[..]), ("pane", &["f5".to_owned()][..]));
    /// ```
    #[must_use]
    pub fn split_at(
        preset: &'a KeymapFile,
        kinds: &[Layer],
        layers: &'a [KeymapFile],
        known_commands: &'a [&'a str],
        screen: Screen,
    ) -> RebindSplit<'a> {
        debug_assert_eq!(
            kinds.len(),
            layers.len(),
            "the kinds are parallel to the layers, one per layer that has a file"
        );
        let n = kinds.len().min(layers.len());
        let cut = kinds[..n]
            .iter()
            .take_while(|k| **k == Layer::System)
            .count();
        // After the system run: at most one `User`, then at most one
        // `Profile`. The target is the LAST of the two that is present, and
        // everything the two of them did not claim goes `below`.
        //
        // The profile step is what D10 (spec 2026-08-26) adds, and it is not a
        // nicety. An active profile carrying a `keymap.toml` sits ABOVE the
        // user layer, so without this the write would land where the profile
        // shadows it and the reader would see a rebind that does nothing.
        // Rebinding a key inside a workspace means it in that workspace.
        let mut below_end = cut;
        let mut target = KeymapFile::default();
        let mut above_from = cut;
        if above_from < n && kinds[above_from] == Layer::User {
            target = layers[above_from].clone();
            above_from += 1;
        }
        if above_from < n && kinds[above_from] == Layer::Profile {
            // The user layer, if there was one, is now BELOW the write: the
            // profile outranks it, exactly as the loader merges them.
            below_end = above_from;
            target = layers[above_from].clone();
            above_from += 1;
        }
        // Everything left above the write must be `Project` and nothing else.
        //
        // Without this the cut would accept orders no resolver produces — a
        // `Profile` before a `User`, say — and target the profile while the
        // user's own layer shadowed it, which is D10's bug mirrored. When the
        // order is not one the loader can emit, the whole stack degrades into
        // `above`: refuse a write that cannot be modelled, never approve one.
        if kinds[above_from..n].iter().any(|k| *k != Layer::Project) {
            below_end = 0;
            target = KeymapFile::default();
            above_from = 0;
        }
        RebindSplit {
            preset,
            below: &layers[..below_end],
            target,
            above: &layers[above_from..],
            known_commands,
            screen,
        }
    }
}

/// The layers cut at the write target ([`RebindSources::split_at`]), owning
/// the target because it may not exist as a file yet.
///
/// Hold it for as long as the [`RebindSources`] borrowed from it
/// ([`Self::sources`]) — which for an editor is the span of one confirm.
#[derive(Debug, Clone)]
pub struct RebindSplit<'a> {
    preset: &'a KeymapFile,
    below: &'a [KeymapFile],
    target: KeymapFile,
    above: &'a [KeymapFile],
    known_commands: &'a [&'a str],
    screen: Screen,
}

impl RebindSplit<'_> {
    /// The borrowed view [`rebind_dry_run`] takes.
    #[must_use]
    pub fn sources(&self) -> RebindSources<'_> {
        RebindSources {
            preset: self.preset,
            below: self.below,
            target: &self.target,
            above: self.above,
            known_commands: self.known_commands,
            screen: self.screen,
        }
    }
}

/// Why a rebind cannot be written.
#[derive(Debug, thiserror::Error)]
pub enum RebindError {
    /// The merged map would not LOAD: the same diagnostic the user would have
    /// met at the next start-up, except that nothing was written.
    #[error(transparent)]
    Load(#[from] KeymapError),
    /// It would load, and the key would keep doing what it does now — the
    /// binding is written, parses, validates and never fires. One cause, since
    /// a twin in the target's own list is repaired rather than refused: a layer
    /// that OUTRANKS the target binds the same sequence, which in practice
    /// means a `./.norte` project layer. Refusing is the only honest answer;
    /// the alternative is an editor that reports success while the key does not
    /// change.
    #[error("the binding would load and never fire: {by:?} keeps that key")]
    Shadowed {
        /// What runs instead.
        by: String,
        /// Whether THAT can run here — so the editor can say "`pane.pack` keeps
        /// that key, and it is not built yet", which is a different sentence
        /// from "`cursor.top` keeps it". Carried for the same reason
        /// [`Rebind::Replaces`] carries it, and carried now because adding it
        /// once c3/c4 match on this variant would be a breaking change.
        avail: Availability,
    },
}

/// The door: what the editor calls immediately before writing, and what it
/// hands to [`norte_config::persist_keymap_bind`].
///
/// It answers the two questions that decide whether a write is worth making,
/// and it answers both by ASKING THE LOADER rather than by re-implementing it:
///
/// 1. **Does it load?** The prospective binding is applied to a copy of
///    `src.target` as [`norte_config::persist_keymap_bind`] applies it —
///    insert-or-replace by chord sequence in `prepend_keymap`, appended at the
///    end when it is new — with one deliberate difference described below; the
///    layers are reassembled in their real order, and the real
///    [`Effective::build_for`] runs. Every rule the loader has today, and every
///    rule it grows tomorrow, is enforced here for free.
/// 2. **Does it take effect?** The built map is then asked what `seq` resolves
///    to. Anything but `command` is [`RebindError::Shadowed`].
///
/// The second question is the one no amount of validation answers, and it is
/// the failure this module exists to stop: a binding that loads and never
/// fires, with the editor reporting success. Two ways in, both invisible to
/// [`rebind_check`], which sees only the merged map.
///
/// One is a differently SPELLED twin of the sequence sitting earlier in the
/// very list being written — `alt+ctrl+p` for what [`Chord`]'s `Display` writes
/// as `ctrl+alt+p` (modifier order), or `mod+p` for `ctrl+p` (the per-OS alias:
/// `mod` resolves to ONE physical modifier, so this pair holds under
/// [`ModKey::Ctrl`](super::ModKey::Ctrl) and is `cmd+p` under the Cmd policy),
/// both perfectly legal to hand-write — which the byte-exact writer would walk
/// past, leaving two entries for one chord and the OLDER one winning. That is
/// the deliberate difference: the
/// match here is by PARSED sequence, so the twin is found, and
/// [`RebindWrite::chords`] carries the spelling ALREADY IN THE FILE so the
/// writer lands on that entry after all. Repaired, not refused.
///
/// The other is a layer that outranks `src.target` — a `./.norte` project
/// layer — binding the same sequence. Nothing the write can do reaches that, so
/// it is [`RebindError::Shadowed`].
///
/// Because the write is modelled and not approximated, a rebind that REPLACES
/// the entry breaking the file succeeds — which matters, since a user whose
/// hand-written binding fails to load has no other way back in. That holds for
/// an entry in the target's `prepend_keymap` for this screen, which is the only
/// place the write reaches: one in `append_keymap`, in `[global]`, or in
/// another layer survives, and the door still refuses, naming it.
///
/// **The verdict is about the layers as they were loaded.** A hand edit or a
/// second norte between this call and the write can still invalidate it; the
/// writer takes the file lock, this does not.
///
/// ```
/// use norte_frontend::keymap::{
///     KeymapFile, RebindSources, Screen, parse_chord, parse_keymap, rebind_dry_run,
/// };
///
/// let preset = parse_keymap("[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n")
///     .unwrap();
/// let none = KeymapFile::default();
/// let src = RebindSources {
///     preset: &preset,
///     below: &[],
///     target: &none,
///     above: &[],
///     known_commands: &["pane.copy", "pane.move"],
///     screen: Screen::Browse,
/// };
/// let w = rebind_dry_run(&src, &[parse_chord("ctrl+j").unwrap()], "pane.move").unwrap();
/// assert_eq!((w.section, &w.chords[..]), ("pane", &["ctrl+j".to_owned()][..]));
/// // Then, off the UI thread (the writer is blocking FS I/O — rule 2):
/// // norte_config::persist_keymap_bind(dir, w.section, w.list, &w.chords, &w.command)
/// ```
///
/// # Errors
/// [`RebindError`] — the load diagnostic, or the binding that would keep the
/// key.
pub fn rebind_dry_run(
    src: &RebindSources<'_>,
    seq: &[Chord],
    command: &str,
) -> Result<RebindWrite, RebindError> {
    // `Display` is what the writer stores and `parse_chord` is what the loader
    // reads back, so rendering here means the round trip a chord actually makes
    // is the one that gets validated — not an approximation of it.
    let rendered: Vec<String> = seq.iter().map(ToString::to_string).collect();
    // The precondition `RebindSources::target` states, in code: a project layer
    // here would model a write into `./.norte` — which the editor never makes —
    // and `merge_ctx` would discard its `lua:` bindings, so the model and the
    // file would disagree about a binding that was never written.
    debug_assert!(
        !src.target.is_project(),
        "the write target is never the project layer"
    );
    let mut target = src.target.clone();
    let list = &mut target.section_mut(src.screen).prepend_keymap;
    // Insert-or-replace, as `persist_keymap_bind` does it — except that the
    // match is by PARSED sequence where the writer's is byte-exact. Same entry
    // in every case the two can both see; where they differ, this finds the
    // twin the writer would have walked past, and hands its spelling back so
    // that the writer lands on it after all.
    let chords = if let Some(existing) = list.iter_mut().find(|b| parses_to(&b.on, seq)) {
        command.clone_into(&mut existing.run);
        existing.on.clone()
    } else {
        list.push(RawBinding {
            on: rendered.clone(),
            run: command.to_owned(),
        });
        rendered
    };
    let mut merged: Vec<KeymapFile> = Vec::with_capacity(src.below.len() + 1 + src.above.len());
    merged.extend_from_slice(src.below);
    merged.push(target);
    merged.extend_from_slice(src.above);
    let after = Effective::build_for(src.preset, &merged, src.known_commands, src.screen)?;
    // `lookup`, not `single_chord_runs`: the latter answers `false` for a
    // multi-chord sequence and for any command this build cannot run, and
    // binding a key to something not built yet is the K2b case, not an error.
    let (by, avail) = match after.lookup(seq) {
        Lookup::Exact(run, avail) => (run, avail),
        // Unreachable: the binding was just put into the map. Treated as a
        // refusal rather than asserted away (rule 6) — from the caller's side
        // "the key does not run your command" is the same answer either way.
        Lookup::Prefix | Lookup::Miss => ("", Availability::Here),
    };
    if by != command {
        return Err(RebindError::Shadowed {
            by: by.to_owned(),
            avail,
        });
    }
    Ok(RebindWrite {
        section: src.screen.section(),
        list: KeymapList::Prepend,
        chords,
        command: command.to_owned(),
    })
}

/// Everything [`norte_config::persist_keymap_unbind`] needs, and what
/// happened — [`RebindWrite`]'s shape, for the removal instead of the write.
///
/// Unlike a bind, a removal is never refused: there is no illegal shape an
/// UNBIND can produce (`persist_keymap_unbind`'s own contract says so), so
/// [`unbind_dry_run`] returns this unconditionally rather than a `Result` with
/// a refusal variant nothing can construct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnbindWrite {
    /// The `[section]` to remove from — this screen's own context
    /// ([`Screen::section`]), never `global`, for the same reason
    /// [`RebindWrite::section`] never is.
    pub section: &'static str,
    /// The chords the target layer's own list spelled the removed entry with
    /// — hand THESE to the writer, not a re-rendering of `seq` — or `seq`
    /// rendered with `Display` when [`Self::outcome`] is
    /// [`UnbindOutcome::NotBound`]: there is nothing in the file to spell
    /// differently, and the caller should not be calling the writer with this
    /// anyway (see that variant).
    pub chords: Vec<String>,
    /// The command the removed entry named, or empty for
    /// [`UnbindOutcome::NotBound`] — nothing was found to name one.
    pub command: String,
    /// What `seq` does now, read off the map WITHOUT the removed entry.
    pub outcome: UnbindOutcome,
}

/// What a sequence does after the removal [`unbind_dry_run`] modelled —
/// `Effective::lookup`'s own answer, never asserted from the file the way
/// the old "removed from your keymap.toml" message did: removing an entry is
/// not the same as the key going quiet (#141).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnbindOutcome {
    /// This screen's own layer had no entry — in `prepend_keymap` OR
    /// `append_keymap` — matching `seq` by PARSED sequence. Three cases look
    /// like this and this door cannot tell them apart, which is why the
    /// wording (`shortcuts-row-global` / `msg-shortcut-nothing-to-unbind`)
    /// is the CALLER's to pick from context, not this type's to guess:
    ///
    /// - the row is a `[global]` binding — [`Screen::section`] never answers
    ///   `"global"`, so this layer's `[pane]`/`[viewer]`/`[dialog]` list was
    ///   never going to have it;
    /// - it lives in another layer entirely (a `./.norte` project layer);
    /// - it really is not bound anywhere the caller should have shown as
    ///   bound — a stale row, or a caller bug.
    ///
    /// Nothing is removed and nothing need be written: the caller should not
    /// call the writer at all, the same way [`ShortcutsState::confirmable`]
    /// gates the bind before it reaches one.
    ///
    /// [`ShortcutsState::confirmable`]: crate::shortcuts::ShortcutsState::confirmable
    NotBound,
    /// The entry was removed, and the sequence now runs nothing at all.
    Cleared,
    /// The entry was removed, and the sequence still runs `command` — the
    /// preset's own binding resurfacing, a lower layer, or a `./.norte`
    /// project layer this editor does not write. The three are worded the
    /// same ("F5 now runs X"): what changed for the reader is identical in
    /// each case, and this door has no way to tell a project layer apart from
    /// an ordinary lower one without asking it twice (see
    /// [`RebindError::Shadowed`] for the one case where the bind path CAN,
    /// and why: writing always makes the target's own entry win unless
    /// something above it does not).
    Runs {
        /// What runs instead.
        command: String,
        /// Whether this build can run it.
        avail: Availability,
    },
}

/// The unbind's door, symmetric with [`rebind_dry_run`] and built out of the
/// same parts: same [`RebindSources`], same parsed-sequence match, same
/// rebuild-and-ask-the-map shape. Three things it fixes over a byte-exact
/// removal (#141, found by the c3 reviewers building the TUI Shortcuts
/// screen):
///
/// 1. **A twin spelling.** `parses_to`, not the writer's byte-exact match,
///    finds a hand-written `mod+p` for a `ctrl+p` row, and
///    [`UnbindWrite::chords`] carries ITS bytes — the same repair
///    [`rebind_dry_run`] already makes for the bind.
/// 2. **`[global]`.** Searched section is `src.screen`'s own
///    ([`Screen::section`]), never `global` — a global binding is
///    [`UnbindOutcome::NotBound`] here, on purpose: this door does not decide
///    what that means, the caller does (see that variant).
/// 3. **The outcome is worded from the REBUILT map**, not from what the file
///    no longer says. "Removed from your keymap.toml" is true and useless
///    when a project layer still binds the key; [`UnbindOutcome`] carries
///    what `Effective::lookup` says instead.
///
/// Searches BOTH of the target's lists (`prepend_keymap` and
/// `append_keymap`) — an unbind has no list of its own to write into the way
/// a rebind always prepends, so either one a hand-written file used is fair
/// game. The removal itself is BYTE-exact on `(chords, command)`, the same
/// predicate [`norte_config::persist_keymap_unbind`]'s own `binding_is` uses
/// — not `parses_to` a second time. A twin spelling under the SAME parsed
/// sequence is a shape the loader shadows rather than rejects (`Effective`'s
/// merge is first-wins, not a load error), so it is exactly the shape this
/// removal must leave standing when the writer would: simulating it as gone
/// because it merely PARSES the same as the removed entry would report a key
/// as silent that the byte-exact writer leaves bound to the twin — the same
/// "editor says one thing, file does another" defect #141 was filed over.
///
/// **The outcome is about the layers AS THEY WERE LOADED**, the same
/// limitation [`rebind_dry_run`] documents for the bind: a hand edit or a
/// second norte between this call and the write can still invalidate it —
/// the writer takes the file lock, this does not — so the wording this
/// returns can describe a map that no longer matches the file by the time
/// the write lands. That window already exists for the bind path; this door
/// inherits it rather than closing it, for the same reason: re-reading and
/// re-building the map here would not make the answer any less stale by the
/// time the write actually runs.
///
/// # Errors
/// [`KeymapError`] if the map without the removed entry still fails to
/// build. Unreachable in practice — removing a binding cannot introduce a
/// prefix clash, a sacred-key violation or an unknown command that were not
/// already there — but the type this door already returns covers it, so no
/// separate error is invented for a wrinkle that cannot happen.
///
/// ```
/// use norte_frontend::keymap::{
///     KeymapFile, RebindSources, Screen, parse_chord, parse_keymap, parse_keymap_layer,
///     unbind_dry_run,
/// };
///
/// let preset = parse_keymap("[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n")
///     .unwrap();
/// // The user rebound F5 under a twin spelling of ctrl+j.
/// let target =
///     parse_keymap_layer("[pane]\nprepend_keymap = [{ on = [\"mod+j\"], run = \"pane.move\" }]\n")
///         .unwrap();
/// let src = RebindSources {
///     preset: &preset,
///     below: &[],
///     target: &target,
///     above: &[],
///     known_commands: &["pane.copy", "pane.move"],
///     screen: Screen::Browse,
/// };
/// let w = unbind_dry_run(&src, &[parse_chord("ctrl+j").unwrap()]).unwrap();
/// // The file's OWN spelling comes back, not `Display`'s `ctrl+j`.
/// assert_eq!(w.chords, vec!["mod+j".to_owned()]);
/// assert_eq!(w.command, "pane.move");
/// ```
pub fn unbind_dry_run(src: &RebindSources<'_>, seq: &[Chord]) -> Result<UnbindWrite, KeymapError> {
    // Same precondition `rebind_dry_run` states, for the same reason: a
    // project layer here would model editing `./.norte`, which the editor
    // never does.
    debug_assert!(
        !src.target.is_project(),
        "the write target is never the project layer"
    );
    let mut target = src.target.clone();
    let section = target.section_mut(src.screen);
    let found = section
        .prepend_keymap
        .iter()
        .find(|b| parses_to(&b.on, seq))
        .or_else(|| section.append_keymap.iter().find(|b| parses_to(&b.on, seq)));
    let Some(existing) = found else {
        let rendered: Vec<String> = seq.iter().map(ToString::to_string).collect();
        return Ok(UnbindWrite {
            section: src.screen.section(),
            chords: rendered,
            command: String::new(),
            outcome: UnbindOutcome::NotBound,
        });
    };
    let chords = existing.on.clone();
    let command = existing.run.clone();
    // BYTE-exact, matching `norte_config`'s own `binding_is` — not
    // `parses_to`. A second, differently-spelled entry that also parses to
    // `seq` (a twin the LOADER shadows rather than rejects, `Effective`'s own
    // dedup is first-wins) must survive this removal exactly as it survives
    // the writer's: modelling it as gone here, when the writer's byte-exact
    // match will leave it standing, is the twin of the bug #141 opened
    // against — a removal the editor reports that the file does not agree
    // with (rust-reviewer BLOCKER, F2).
    section
        .prepend_keymap
        .retain(|b| !(b.on == chords && b.run == command));
    section
        .append_keymap
        .retain(|b| !(b.on == chords && b.run == command));
    let mut merged: Vec<KeymapFile> = Vec::with_capacity(src.below.len() + 1 + src.above.len());
    merged.extend_from_slice(src.below);
    merged.push(target);
    merged.extend_from_slice(src.above);
    let after = Effective::build_for(src.preset, &merged, src.known_commands, src.screen)?;
    let outcome = match after.lookup(seq) {
        Lookup::Exact(run, avail) => UnbindOutcome::Runs {
            command: run.to_owned(),
            avail,
        },
        Lookup::Prefix | Lookup::Miss => UnbindOutcome::Cleared,
    };
    Ok(UnbindWrite {
        section: src.screen.section(),
        chords,
        command,
        outcome,
    })
}

/// Do these written chords parse to exactly `seq`? The question
/// [`norte_config::persist_keymap_bind`] cannot ask (it is below the grammar
/// and compares bytes), and the reason a hand-written `mod+p` does not become
/// a second, winning copy of the key being rebound.
fn parses_to(on: &[String], seq: &[Chord]) -> bool {
    on.len() == seq.len()
        && on
            .iter()
            .zip(seq)
            .all(|(s, c)| parse_chord(s).is_ok_and(|p| p == *c))
}

/// Does this chord survive the trip through the file? `Display` writes it,
/// [`parse_chord`] reads it back, and the two must produce the same chord —
/// the round trip a binding makes between the editor and the next start-up.
fn round_trips(c: Chord) -> bool {
    parse_chord(&c.to_string()).is_ok_and(|back| back == c)
}

#[cfg(test)]
mod tests {
    use norte_config::{Layer, Layers};

    use super::{
        Rebind, RebindError, RebindSources, RebindWrite, UnbindOutcome, rebind_check,
        rebind_dry_run, unbind_dry_run,
    };
    use crate::keymap::{
        Availability, Chord, Effective, KeyCode, KeymapFile, Mods, Screen, parse_chord,
        parse_keymap,
    };

    /// A vim-shaped browse map: a whole binding, a two-chord sequence, a key
    /// bound to something this build has not got (`pane.pack` is `Planned`,
    /// #132), and `tab` left to the specification.
    const BROWSE: &str = r#"
[pane]
keymap = [
    { on = ["f5"], run = "pane.copy" },
    { on = ["alt+f5"], run = "pane.pack" },
    { on = ["g", "g"], run = "cursor.top" },
    { on = ["esc"], run = "mark.clear" },
]
[viewer]
keymap = [{ on = ["ctrl+w"], run = "viewer.close" }]
"#;

    const KNOWN: &[&str] = &[
        "pane.copy",
        "cursor.top",
        "mark.clear",
        "viewer.close",
        "pane.move",
        "dialog.approve",
        // The reserved command, needed by the one test that binds it.
        "pane.switch",
    ];

    fn eff(src: &str, screen: Screen) -> Effective {
        let preset = parse_keymap(src).expect("fixture parses");
        Effective::build_for(&preset, &[], KNOWN, screen).expect("fixture builds")
    }

    fn c(s: &str) -> Chord {
        parse_chord(s).expect("chord")
    }

    #[test]
    fn a_key_nobody_uses_is_free() {
        assert_eq!(
            rebind_check(&eff(BROWSE, Screen::Browse), &[c("ctrl+j")]),
            Rebind::Free
        );
    }

    #[test]
    fn an_exact_match_is_replaces_with_what_it_runs() {
        assert_eq!(
            rebind_check(&eff(BROWSE, Screen::Browse), &[c("f5")]),
            Rebind::Replaces {
                command: "pane.copy".to_owned(),
                avail: Availability::Here,
            }
        );
    }

    /// The K2b case: the key the user wants is bound to something this build
    /// cannot run, and the editor has to be able to SAY so — which is the
    /// whole reason `Replaces` carries the availability.
    ///
    /// It was a `Planned` command until #132 built the last of them; what a
    /// key can now be bound to and still not run is a command this frontend
    /// does not implement.
    #[test]
    fn replaces_carries_the_availability_of_what_it_displaces() {
        let v = rebind_check(&eff(BROWSE, Screen::Browse), &[c("alt+f5")]);
        assert_eq!(
            v,
            Rebind::Replaces {
                command: "pane.pack".to_owned(),
                avail: Availability::NotHere,
            }
        );
        assert!(
            !v.is_refusal(),
            "a key that does nothing is still free to take"
        );
    }

    /// Both directions of ADR 0006's rule: the new sequence swallowing an
    /// existing one, and an existing one swallowing the new.
    #[test]
    fn a_prefix_either_way_is_a_clash() {
        let e = eff(BROWSE, Screen::Browse);
        let shorter = rebind_check(&e, &[c("g")]);
        assert_eq!(
            shorter,
            Rebind::PrefixClash {
                with: "g g".to_owned(),
                command: "cursor.top".to_owned(),
            }
        );
        let longer = rebind_check(&e, &[c("g"), c("g"), c("h")]);
        assert_eq!(
            longer,
            Rebind::PrefixClash {
                with: "g g".to_owned(),
                command: "cursor.top".to_owned(),
            }
        );
        assert!(shorter.is_refusal() && longer.is_refusal());
    }

    /// §12 / ADR 0044: the rule is about the FIRST chord, so a sequence that
    /// merely STARTS with Tab is refused too — leaving Tab pending loses pane
    /// switching just as completely as rebinding it.
    #[test]
    fn tab_is_sacred_in_browse_first_chord_included() {
        let e = eff(BROWSE, Screen::Browse);
        let expected = Rebind::Sacred {
            reserved_for: "pane.switch",
        };
        assert_eq!(rebind_check(&e, &[c("tab")]), expected);
        assert_eq!(rebind_check(&e, &[c("tab"), c("j")]), expected);
    }

    /// And ONLY in Browse: every bundled preset binds `tab` inside `[dialog]`,
    /// where it is not pane switching. There it is an ordinary binding — which
    /// also shows the verdict is read off the map's own screen, not guessed.
    #[test]
    fn tab_is_an_ordinary_key_in_a_dialog() {
        let src = "[dialog]\nkeymap = [{ on = [\"tab\"], run = \"dialog.approve\" }]\n";
        let e = eff(src, Screen::Dialog);
        assert_eq!(e.screen(), Screen::Dialog);
        assert_eq!(
            rebind_check(&e, &[c("tab")]),
            Rebind::Replaces {
                command: "dialog.approve".to_owned(),
                avail: Availability::Here,
            }
        );
    }

    /// K2a: with counts on, 1-9 cannot also open a binding — and `0` can,
    /// because a count never starts with zero.
    #[test]
    fn a_digit_is_refused_only_where_counts_claim_it() {
        let counting = format!("counts = true\n{BROWSE}");
        let with = eff(&counting, Screen::Browse);
        assert_eq!(rebind_check(&with, &[c("5")]), Rebind::DigitWithCounts);
        assert_eq!(
            rebind_check(&with, &[c("0")]),
            Rebind::Free,
            "el 0 sí es ligable"
        );
        assert_eq!(
            rebind_check(&with, &[c("ctrl+5")]),
            Rebind::Free,
            "un dígito con modificador jamás fue un contador"
        );
        let without = eff(BROWSE, Screen::Browse);
        assert_eq!(rebind_check(&without, &[c("5")]), Rebind::Free);
    }

    /// The case the plan asks for by name: a chord bound in another screen
    /// only is not a collision here.
    #[test]
    fn a_chord_bound_only_in_another_screen_is_free() {
        let browse = eff(BROWSE, Screen::Browse);
        assert_eq!(rebind_check(&browse, &[c("ctrl+w")]), Rebind::Free);
        // And it IS taken over there, so the fixture is not vacuous.
        let viewer = eff(BROWSE, Screen::Viewer);
        assert!(matches!(
            rebind_check(&viewer, &[c("ctrl+w")]),
            Rebind::Replaces { .. }
        ));
    }

    #[test]
    fn an_empty_capture_is_not_free() {
        assert_eq!(
            rebind_check(&eff(BROWSE, Screen::Browse), &[]),
            Rebind::Empty
        );
    }

    /// `esc` cancels a pending prefix, so it is unreachable inside a sequence
    /// — but perfectly good alone, and the fixture binds it that way.
    #[test]
    fn esc_is_refused_inside_a_sequence_and_fine_alone() {
        let e = eff(BROWSE, Screen::Browse);
        assert_eq!(rebind_check(&e, &[c("g"), c("esc")]), Rebind::EscInSequence);
        assert_eq!(
            rebind_check(&e, &[c("esc")]),
            Rebind::Replaces {
                command: "mark.clear".to_owned(),
                avail: Availability::Here,
            }
        );
    }

    /// A chord no `keymap.toml` can express: `Display` writes `f13` and
    /// `parse_chord` refuses it, so writing it would revert the user's whole
    /// keymap on the next reload. Both frontends clamp F-keys to 1..=12 before
    /// they reach here — this is the belt.
    #[test]
    fn a_chord_that_does_not_round_trip_is_unwritable() {
        let f13 = Chord::new(Mods::default(), KeyCode::F(13));
        assert_eq!(
            rebind_check(&eff(BROWSE, Screen::Browse), &[f13]),
            Rebind::Unwritable {
                chord: "F13".to_owned()
            }
        );
    }

    /// The sources a lone user layer makes: no system layer below, no project
    /// layer above.
    fn only_user<'a>(
        preset: &'a KeymapFile,
        target: &'a KeymapFile,
        screen: Screen,
    ) -> RebindSources<'a> {
        RebindSources {
            preset,
            below: &[],
            target,
            above: &[],
            known_commands: KNOWN,
            screen,
        }
    }

    /// Writes what the door approved and loads it back through the REAL
    /// loader, returning the effective map of `screen` — the round trip that
    /// decides whether the user's keymap survived.
    fn write_and_reload(preset: &KeymapFile, w: &RebindWrite, screen: Screen) -> Effective {
        let dir = tempfile::tempdir().expect("tmp");
        norte_config::persist_keymap_bind(dir.path(), w.section, w.list, &w.chords, &w.command)
            .expect("write");
        let cfg = crate::config::load(&Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        })
        .expect("what the dry run approved LOADS");
        Effective::build_for(preset, &cfg.keymap_layers, KNOWN, screen).expect("and builds")
    }

    /// End to end, which is the only thing that proves the door is a door:
    /// dry run → write → LOAD through the real loader → the binding resolves.
    /// Every argument the writer takes comes out of `RebindWrite`, so a section
    /// or a list chosen wrongly would show up here as a keymap that reverted.
    #[test]
    fn what_the_dry_run_returns_writes_loads_and_resolves() {
        let preset = parse_keymap(BROWSE).expect("preset");
        let none = KeymapFile::default();
        let w = rebind_dry_run(
            &only_user(&preset, &none, Screen::Browse),
            &[c("ctrl+j")],
            "pane.move",
        )
        .expect("a free chord passes the loader");
        assert_eq!(
            w,
            RebindWrite {
                section: "pane",
                list: norte_config::KeymapList::Prepend,
                chords: vec!["ctrl+j".to_owned()],
                command: "pane.move".to_owned(),
            }
        );
        let after = write_and_reload(&preset, &w, Screen::Browse);
        assert!(after.single_chord_runs(c("ctrl+j"), "pane.move"));
    }

    /// And the prepend really is what a REBIND needs: the same round trip over
    /// a key the preset already binds takes it over.
    #[test]
    fn a_rebind_of_a_taken_key_wins_after_the_write() {
        let preset = parse_keymap(BROWSE).expect("preset");
        let none = KeymapFile::default();
        let seq = [c("f5")];
        assert!(matches!(
            rebind_check(&eff(BROWSE, Screen::Browse), &seq),
            Rebind::Replaces { .. }
        ));
        let w = rebind_dry_run(
            &only_user(&preset, &none, Screen::Browse),
            &seq,
            "pane.move",
        )
        .expect("replacing a binding is legal");
        assert!(
            write_and_reload(&preset, &w, Screen::Browse).single_chord_runs(c("f5"), "pane.move"),
            "el rebind gana"
        );
    }

    /// The BLOCKER this door exists for. The user's own layer already binds the
    /// chord under another legal spelling — `alt+ctrl+p` for the `ctrl+alt+p`
    /// `Display` writes (modifier order), and `mod+p` for `ctrl+p` (the per-OS
    /// alias, under this process's `ModKey::Ctrl`). The writer matches bytes,
    /// so it would have appended a SECOND entry, and the first one wins:
    /// the file loads, the editor reports success, and the key keeps doing the
    /// old thing forever — the second rebind cannot fix it either. The door
    /// hands back the spelling that is already in the file, so the write lands
    /// on the entry that actually wins.
    #[test]
    fn a_twin_spelling_in_the_target_layer_is_rebound_in_place() {
        let preset = parse_keymap(BROWSE).expect("preset");
        for spelling in ["alt+ctrl+p", "mod+p"] {
            let target = crate::keymap::parse_keymap_layer(&format!(
                "[pane]\nprepend_keymap = [{{ on = [\"{spelling}\"], run = \"cursor.top\" }}]\n"
            ))
            .expect("the user's own layer");
            let seq = [c(spelling)];
            let w = rebind_dry_run(
                &only_user(&preset, &target, Screen::Browse),
                &seq,
                "pane.move",
            )
            .expect("rebinding a key you already bound is legal");
            assert_eq!(
                w.chords,
                vec![spelling.to_owned()],
                "the spelling ALREADY IN THE FILE, not `Display`'s"
            );

            // And end to end, against the real writer: one entry, rebound.
            let dir = tempfile::tempdir().expect("tmp");
            std::fs::write(
                dir.path().join("keymap.toml"),
                format!(
                    "[pane]\nprepend_keymap = [{{ on = [\"{spelling}\"], run = \"cursor.top\" }}]\n"
                ),
            )
            .expect("seed");
            norte_config::persist_keymap_bind(dir.path(), w.section, w.list, &w.chords, &w.command)
                .expect("write");
            let cfg = crate::config::load(&Layers {
                dirs: vec![(dir.path().to_path_buf(), Layer::User)],
            })
            .expect("loads");
            let after = Effective::build_for(&preset, &cfg.keymap_layers, KNOWN, Screen::Browse)
                .expect("builds");
            assert!(
                after.single_chord_runs(c(spelling), "pane.move"),
                "{spelling}: el rebind se escribió y NO disparó"
            );
        }
    }

    /// A layer that outranks the target keeps the key whatever the user writes:
    /// a project `./.norte/keymap.toml` prepends ahead of the user's. It loads
    /// — so no `KeymapError` will ever mention it — and it never fires, which
    /// is the failure `Shadowed` exists to name.
    #[test]
    fn a_layer_above_the_target_makes_the_write_inert() {
        let preset = parse_keymap(BROWSE).expect("preset");
        let none = KeymapFile::default();
        let project = crate::keymap::parse_keymap_layer(
            "[pane]\nprepend_keymap = [{ on = [\"ctrl+j\"], run = \"cursor.top\" }]\n",
        )
        .expect("project layer");
        let src = RebindSources {
            above: std::slice::from_ref(&project),
            ..only_user(&preset, &none, Screen::Browse)
        };
        let err = rebind_dry_run(&src, &[c("ctrl+j")], "pane.move")
            .expect_err("a write nobody would ever see is not a write worth making");
        assert!(
            matches!(
                &err,
                RebindError::Shadowed { by, avail: Availability::Here } if by == "cursor.top"
            ),
            "{err:?}"
        );
        // Below the target it is the ordinary case: the user's rebind wins.
        let src = RebindSources {
            below: std::slice::from_ref(&project),
            ..only_user(&preset, &none, Screen::Browse)
        };
        rebind_dry_run(&src, &[c("ctrl+j")], "pane.move").expect("a lower layer loses, as it must");
    }

    /// J1, first miscut: the user has no `keymap.toml` and the system dir
    /// does, so `keymap_layers` is `[system]` — and a caller taking "the last
    /// non-project layer" as the target would hand over the SYSTEM layer. The
    /// tell is that the system file's own broken entry would then look
    /// REPAIRABLE, when the write only ever reaches the user's dir; and the
    /// spelling handed back would be the system file's rather than the new
    /// layer's. `split_at` puts it in `below`, where it belongs.
    #[test]
    fn a_system_only_stack_targets_a_new_user_layer() {
        let preset = parse_keymap(BROWSE).expect("preset");
        let broken = crate::keymap::parse_keymap_layer(
            "[pane]\nprepend_keymap = [{ on = [\"ctrl+j\"], run = \"pane.telport\" }]\n",
        )
        .expect("it parses; the COMMAND is the typo");
        let layers = [broken];
        let kinds = [Layer::System];
        let split = RebindSources::split_at(&preset, &kinds, &layers, KNOWN, Screen::Browse);
        assert_eq!(split.below.len(), 1, "el sistema va DEBAJO de la escritura");
        assert!(split.above.is_empty());
        assert!(!split.target.is_project());
        let err = rebind_dry_run(&split.sources(), &[c("ctrl+j")], "pane.move")
            .expect_err("a user rebind cannot repair the SYSTEM file");
        assert!(
            matches!(
                &err,
                RebindError::Load(crate::keymap::KeymapError::UnknownCommand { run })
                    if run == "pane.telport"
            ),
            "{err:?}"
        );

        // And with a system layer that loads: the write wins over it, and the
        // spelling is `Display`'s — the target list is EMPTY, so the system
        // file's `mod+j` is not a twin the write may land on.
        let system = crate::keymap::parse_keymap_layer(
            "[pane]\nprepend_keymap = [{ on = [\"mod+j\"], run = \"cursor.top\" }]\n",
        )
        .expect("system layer");
        let layers = [system];
        let split = RebindSources::split_at(&preset, &kinds, &layers, KNOWN, Screen::Browse);
        let w = rebind_dry_run(&split.sources(), &[c("ctrl+j")], "pane.move")
            .expect("the user's own layer outranks the system one");
        assert_eq!(
            w.chords,
            vec!["ctrl+j".to_owned()],
            "la capa destino está vacía: se escribe la grafía de `Display`"
        );
    }

    /// D10: with a profile active, the shortcut is written INTO the profile.
    ///
    /// Writing it into the user layer would leave it shadowed by the profile's
    /// own `keymap.toml`, and the reader would see a rebind that does nothing.
    /// So the cut widens by one step and the target is the LAST of
    /// `User`/`Profile` present.
    #[test]
    fn con_perfil_activo_el_destino_es_el_perfil() {
        let preset = parse_keymap(BROWSE).expect("preset");
        let user = crate::keymap::parse_keymap_layer(
            "[pane]\nprepend_keymap = [{ on = [\"mod+k\"], run = \"cursor.top\" }]\n",
        )
        .expect("user layer");
        let profile = crate::keymap::parse_keymap_layer(
            "[pane]\nprepend_keymap = [{ on = [\"ctrl+j\"], run = \"cursor.top\" }]\n",
        )
        .expect("profile layer");
        let layers = [user, profile];
        let kinds = [Layer::User, Layer::Profile];
        let split = RebindSources::split_at(&preset, &kinds, &layers, KNOWN, Screen::Browse);
        assert_eq!(
            split.below.len(),
            1,
            "la capa del usuario queda DEBAJO de la escritura"
        );
        assert!(
            split.above.is_empty(),
            "nada por encima del destino: el perfil ES el destino"
        );
        // Y el destino es el fichero DEL PERFIL, no uno vacío: rebindear su
        // propia entrada la reemplaza en sitio, con su grafía.
        let w = rebind_dry_run(&split.sources(), &[c("ctrl+j")], "pane.move")
            .expect("se escribe en el perfil");
        assert_eq!(w.chords, vec!["ctrl+j".to_owned()]);
    }

    /// Un perfil SIN `keymap.toml` propio no cambia el destino: sigue siendo la
    /// capa del usuario, y la del perfil ni siquiera está en `kinds`.
    #[test]
    fn sin_perfil_el_destino_sigue_siendo_el_usuario() {
        let preset = parse_keymap(BROWSE).expect("preset");
        let system = crate::keymap::parse_keymap_layer(
            "[pane]\nprepend_keymap = [{ on = [\"mod+j\"], run = \"cursor.top\" }]\n",
        )
        .expect("system layer");
        let layers = [system];
        let kinds = [Layer::System];
        let split = RebindSources::split_at(&preset, &kinds, &layers, KNOWN, Screen::Browse);
        assert_eq!(split.below.len(), 1);
        assert!(split.above.is_empty());
        let w = rebind_dry_run(&split.sources(), &[c("ctrl+j")], "pane.move")
            .expect("la capa del usuario sigue siendo el destino");
        assert_eq!(w.chords, vec!["ctrl+j".to_owned()]);
    }

    /// Y cualquier OTRO orden sigue cayendo en `above`, que es el lado
    /// fail-closed: la puerta rehúsa lo que no sabe modelar, nunca aprueba lo
    /// que no aprobó. Un perfil DEBAJO del usuario no es un orden que ningún
    /// resolutor produzca.
    #[test]
    fn un_orden_inesperado_sigue_siendo_fail_closed() {
        let preset = parse_keymap(BROWSE).expect("preset");
        let a = crate::keymap::parse_keymap_layer("[pane]\nprepend_keymap = []\n").expect("a");
        let b = crate::keymap::parse_keymap_layer("[pane]\nprepend_keymap = []\n").expect("b");
        let layers = [a, b];
        let kinds = [Layer::Profile, Layer::User];
        let split = RebindSources::split_at(&preset, &kinds, &layers, KNOWN, Screen::Browse);
        assert!(split.below.is_empty());
        assert_eq!(split.above.len(), 2, "todo por encima; no se escribe nada");
    }

    /// J1, second miscut: a project layer is LAST in `keymap_layers`, so a
    /// caller taking "the last layer" as the target would model the write
    /// ABOVE the user's own — and `Shadowed`, the whole reason the split
    /// exists, would stop firing. `split_at` puts it in `above` and keeps the
    /// USER layer as the target, which the second half checks is not merely
    /// `KeymapFile::default()`: rebinding the user's own entry replaces it in
    /// place.
    #[test]
    fn a_project_layer_goes_above_the_write_and_the_user_layer_stays_the_target() {
        let preset = parse_keymap(BROWSE).expect("preset");
        let user = crate::keymap::parse_keymap_layer(
            "[pane]\nprepend_keymap = [{ on = [\"mod+k\"], run = \"cursor.top\" }]\n",
        )
        .expect("user layer");
        let mut project = crate::keymap::parse_keymap_layer(
            "[pane]\nprepend_keymap = [{ on = [\"ctrl+j\"], run = \"cursor.top\" }]\n",
        )
        .expect("project layer");
        project.mark_project();
        let layers = [user, project];
        let kinds = [Layer::User, Layer::Project];
        let split = RebindSources::split_at(&preset, &kinds, &layers, KNOWN, Screen::Browse);
        assert!(split.below.is_empty());
        assert_eq!(split.above.len(), 1, "el proyecto manda ENCIMA");
        assert!(
            !split.target.is_project(),
            "el destino jamás es la capa de proyecto"
        );
        let err = rebind_dry_run(&split.sources(), &[c("ctrl+j")], "pane.move")
            .expect_err("the project layer keeps that key whatever the user writes");
        assert!(matches!(&err, RebindError::Shadowed { .. }), "{err:?}");
        // The target really is the USER layer and not an empty one: its own
        // entry is rebound in place, spelling included.
        let w = rebind_dry_run(&split.sources(), &[c("ctrl+k")], "pane.move")
            .expect("rebinding your own binding is legal");
        assert_eq!(
            w.chords,
            vec!["mod+k".to_owned()],
            "la grafía YA EN EL FICHERO del usuario: el destino es su capa"
        );
    }

    /// `Shadowed` carries the availability for the same reason `Replaces`
    /// does: "`pane.pack` keeps that key, and this build cannot run it" is a
    /// different sentence from "`cursor.top` keeps it", and only the editor
    /// that can say which one avoids sending a user to look for a feature
    /// that is not there.
    ///
    /// It used to say "and it is not built yet": #132 built the last `Planned`
    /// command in the catalogue, so the availability that a shadow can carry
    /// today is `NotHere` — the command exists and this frontend does not run
    /// it. The point of the field is unchanged.
    #[test]
    fn shadowed_says_whether_what_keeps_the_key_even_works() {
        let preset = parse_keymap(BROWSE).expect("preset");
        let none = KeymapFile::default();
        let project = crate::keymap::parse_keymap_layer(
            "[pane]\nprepend_keymap = [{ on = [\"ctrl+j\"], run = \"pane.pack\" }]\n",
        )
        .expect("project layer");
        let src = RebindSources {
            above: std::slice::from_ref(&project),
            ..only_user(&preset, &none, Screen::Browse)
        };
        let err = rebind_dry_run(&src, &[c("ctrl+j")], "pane.move").expect_err("shadowed");
        assert!(
            matches!(
                &err,
                RebindError::Shadowed {
                    by,
                    avail: Availability::NotHere,
                } if by == "pane.pack"
            ),
            "{err:?}"
        );
    }

    /// Each screen's binding lands in its OWN section and is invisible to the
    /// other two: `Screen::section` and the reader's `Screen::specific` are the
    /// same mapping, and this is what says so from outside the module.
    #[test]
    fn a_binding_lands_in_its_screen_section_and_nowhere_else() {
        let preset = parse_keymap(BROWSE).expect("preset");
        let none = KeymapFile::default();
        for screen in [Screen::Browse, Screen::Viewer, Screen::Dialog] {
            let w = rebind_dry_run(
                &only_user(&preset, &none, screen),
                &[c("ctrl+j")],
                "pane.move",
            )
            .expect("free everywhere");
            assert_eq!(w.section, screen.section());
            let dir = tempfile::tempdir().expect("tmp");
            norte_config::persist_keymap_bind(dir.path(), w.section, w.list, &w.chords, &w.command)
                .expect("write");
            let cfg = crate::config::load(&Layers {
                dirs: vec![(dir.path().to_path_buf(), Layer::User)],
            })
            .expect("loads");
            for other in [Screen::Browse, Screen::Viewer, Screen::Dialog] {
                let e = Effective::build_for(&preset, &cfg.keymap_layers, KNOWN, other)
                    .expect("builds");
                assert_eq!(
                    e.single_chord_runs(c("ctrl+j"), "pane.move"),
                    other == screen,
                    "{screen:?} escribió en {}, y {other:?} discrepa",
                    w.section
                );
            }
        }
    }

    /// The belt earning its keep: the CHORD is free — `rebind_check` says so
    /// truthfully, it never sees the command — and the write would still have
    /// produced a layer that does not load.
    #[test]
    fn the_dry_run_refuses_a_command_the_chord_check_cannot_see() {
        let preset = parse_keymap(BROWSE).expect("preset");
        let none = KeymapFile::default();
        let seq = [c("ctrl+j")];
        assert_eq!(
            rebind_check(&eff(BROWSE, Screen::Browse), &seq),
            Rebind::Free
        );
        let err = rebind_dry_run(
            &only_user(&preset, &none, Screen::Browse),
            &seq,
            "pane.teleport",
        )
        .expect_err("a command no catalogue knows cannot be written");
        assert!(
            matches!(
                &err,
                RebindError::Load(crate::keymap::KeymapError::UnknownCommand { run })
                    if run == "pane.teleport"
            ),
            "{err:?}"
        );
    }

    /// A layer that already does not load: the rebind that REPAIRS the
    /// offending entry goes through — the writer replaces it in place, and it
    /// is the only way back in for a user whose hand-written binding broke
    /// their keymap — while a rebind that leaves it alone still fails, naming
    /// it. This is why the door models the write instead of appending a layer:
    /// the coarser answer would have locked the editor exactly when it is
    /// needed.
    #[test]
    fn a_broken_layer_can_be_repaired_by_rebinding_the_entry_that_breaks_it() {
        let preset = parse_keymap(BROWSE).expect("preset");
        let broken = crate::keymap::parse_keymap_layer(
            "[pane]\nprepend_keymap = [{ on = [\"ctrl+j\"], run = \"pane.telport\" }]\n",
        )
        .expect("it parses; it is the COMMAND that does not exist");
        let src = only_user(&preset, &broken, Screen::Browse);
        rebind_dry_run(&src, &[c("ctrl+j")], "pane.move").expect("rebinding it IS the repair");
        let err = rebind_dry_run(&src, &[c("ctrl+k")], "pane.move")
            .expect_err("another key leaves the typo in place");
        assert!(
            matches!(
                &err,
                RebindError::Load(crate::keymap::KeymapError::UnknownCommand { run })
                    if run == "pane.telport"
            ),
            "{err:?}"
        );
    }

    /// On whether a SEQUENCE may be bound, the classifier and the loader agree
    /// — variant for variant, not merely yes/no, and across every screen, with
    /// and without counts, with a layer in play. This is what makes "show the
    /// verdict, then run the door" safe in both directions: without it the
    /// editor could refuse a legal binding, or promise one the write would
    /// revert. The two known disagreements are elsewhere and deliberate: the
    /// sacred key (below) and the questions the classifier cannot see at all
    /// (the command, and whether the binding fires).
    #[test]
    fn the_classifier_and_the_loader_agree() {
        let layer = crate::keymap::parse_keymap_layer(
            "[pane]\nprepend_keymap = [{ on = [\"ctrl+x\", \"a\"], run = \"pane.copy\" }]\n",
        )
        .expect("layer");
        let cases: Vec<Vec<Chord>> = vec![
            vec![c("ctrl+j")],
            vec![c("f5")],
            vec![c("alt+f5")],
            vec![c("g")],
            vec![c("g"), c("g"), c("h")],
            vec![c("g"), c("h")],
            vec![c("ctrl+x")],
            vec![c("ctrl+x"), c("a")],
            vec![c("ctrl+w")],
            vec![c("5")],
            vec![c("0")],
            vec![c("esc")],
            vec![c("g"), c("esc")],
            vec![c("tab"), c("j")],
            vec![Chord::new(Mods::default(), KeyCode::F(13))],
            vec![],
        ];
        for counts in [false, true] {
            let src = if counts {
                format!("counts = true\n{BROWSE}")
            } else {
                BROWSE.to_owned()
            };
            let preset = parse_keymap(&src).expect("preset");
            for screen in [Screen::Browse, Screen::Viewer, Screen::Dialog] {
                let e = Effective::build_for(&preset, std::slice::from_ref(&layer), KNOWN, screen)
                    .expect("build");
                for seq in &cases {
                    let verdict = rebind_check(&e, seq);
                    let sources = RebindSources {
                        preset: &preset,
                        below: &[],
                        target: &layer,
                        above: &[],
                        known_commands: KNOWN,
                        screen,
                    };
                    let door = rebind_dry_run(&sources, seq, "pane.move");
                    assert_eq!(
                        verdict.is_refusal(),
                        door.is_err(),
                        "counts={counts} {screen:?} {seq:?}: {verdict:?} vs {door:?}"
                    );
                    // And the SAME defect, not merely some defect.
                    let want = match &verdict {
                        Rebind::PrefixClash { .. } => Some("secuencias ambiguas"),
                        Rebind::DigitWithCounts => Some("contador"),
                        Rebind::EscInSequence => Some("esc"),
                        Rebind::Empty => Some("vacía"),
                        Rebind::Unwritable { .. } => Some("tecla inválida"),
                        Rebind::Sacred { .. } | Rebind::Free | Rebind::Replaces { .. } => None,
                    };
                    if let (Some(want), Err(e)) = (want, &door) {
                        assert!(
                            e.to_string().contains(want),
                            "counts={counts} {screen:?} {seq:?}: {verdict:?} vs {e}"
                        );
                    }
                }
            }
        }
    }

    /// The one place the two differ ON A SEQUENCE, pinned so it cannot quietly
    /// become something else: the loader accepts `tab` bound to the very
    /// command it is reserved for, and the editor refuses it anyway. Nothing is
    /// lost — that binding already exists — and the rule stays statable without
    /// the command.
    #[test]
    fn the_sacred_key_is_refused_even_where_the_loader_would_take_it() {
        let preset = parse_keymap(BROWSE).expect("preset");
        let none = KeymapFile::default();
        let src = only_user(&preset, &none, Screen::Browse);
        let seq = [c("tab")];
        assert!(rebind_check(&eff(BROWSE, Screen::Browse), &seq).is_refusal());
        rebind_dry_run(&src, &seq, "pane.switch")
            .expect("the LOADER accepts the reserved binding itself");
        // Bound to anything else, both refuse.
        assert!(
            rebind_dry_run(&src, &seq, "pane.move").is_err(),
            "tab bound elsewhere is a load error"
        );
    }

    /// The bind path already repairs a twin spelling — `rebind_dry_run` hands
    /// the writer the spelling that is IN the file. The unbind had no
    /// equivalent, so a hand-written `mod+p` survived an unbind of `ctrl+p`
    /// and the key kept firing (#141).
    #[test]
    fn an_unbind_finds_a_twin_spelling_and_returns_the_files_own() {
        let preset = parse_keymap(BROWSE).expect("preset");
        for spelling in ["alt+ctrl+p", "mod+p"] {
            let target = crate::keymap::parse_keymap_layer(&format!(
                "[pane]\nprepend_keymap = [{{ on = [\"{spelling}\"], run = \"pane.move\" }}]\n"
            ))
            .expect("the user's own layer");
            let seq = [c(spelling)];
            let src = only_user(&preset, &target, Screen::Browse);
            let w = unbind_dry_run(&src, &seq).expect("removing a real entry always succeeds");
            assert_eq!(
                w.chords,
                vec![spelling.to_owned()],
                "the spelling ALREADY IN THE FILE, not `Display`'s"
            );
            assert_eq!(w.command, "pane.move");
            assert_eq!(
                w.outcome,
                UnbindOutcome::Cleared,
                "{spelling}: nothing else in BROWSE binds it"
            );

            // And end to end, against the real writer: the twin is gone and
            // the byte-exact writer lands on the entry `Display` would have
            // walked past.
            let dir = tempfile::tempdir().expect("tmp");
            std::fs::write(
                dir.path().join("keymap.toml"),
                format!(
                    "[pane]\nprepend_keymap = [{{ on = [\"{spelling}\"], run = \"pane.move\" }}]\n"
                ),
            )
            .expect("seed");
            let removed =
                norte_config::persist_keymap_unbind(dir.path(), w.section, &w.chords, &w.command)
                    .expect("write");
            assert!(removed.changed, "{spelling}: the twin was not found");
            let cfg = crate::config::load(&Layers {
                dirs: vec![(dir.path().to_path_buf(), Layer::User)],
            })
            .expect("loads");
            let after = Effective::build_for(&preset, &cfg.keymap_layers, KNOWN, Screen::Browse)
                .expect("builds");
            assert!(
                !after
                    .bindings_all_seq()
                    .into_iter()
                    .any(|(s, _, _)| s == seq.as_slice()),
                "{spelling}: el gemelo debía desaparecer"
            );
        }
    }

    /// rust-reviewer BLOCKER (F2): a twin spelling the LOADER shadows rather
    /// than rejects — TWO entries in the same list that parse to the same
    /// chord, a legal and loadable shape (`Effective`'s dedup is first-wins,
    /// it is not a load error) — must not both disappear from the
    /// SIMULATION when only one disappears from the FILE. `persist_keymap_unbind`
    /// removes byte-exact matches of `(chords, command)`, so simulating a
    /// removal of every sequence-alike entry (the bug: matching by
    /// `parses_to` instead of by the returned bytes) predicted `Cleared` for
    /// a key the real writer leaves bound to the shadowed twin — the same
    /// "the editor says one thing, the file does another" defect #141 was
    /// filed over, reintroduced through the new door.
    #[test]
    fn an_unbind_leaves_a_differently_spelled_twin_of_the_removed_entry_standing() {
        let preset = parse_keymap(BROWSE).expect("preset");
        let target = crate::keymap::parse_keymap_layer(
            "[pane]\nprepend_keymap = [\n\
                { on = [\"ctrl+p\"], run = \"pane.move\" },\n\
                { on = [\"mod+p\"], run = \"cursor.top\" },\n\
             ]\n",
        )
        .expect("two entries, one parsed sequence — the loader shadows, it does not reject");
        let seq = [c("ctrl+p")];
        let src = only_user(&preset, &target, Screen::Browse);
        let w = unbind_dry_run(&src, &seq).expect("removing the winning entry always succeeds");
        // The WINNING entry (first in the list) is the one removed.
        assert_eq!(w.chords, vec!["ctrl+p".to_owned()]);
        assert_eq!(w.command, "pane.move");
        // And the shadowed twin — a DIFFERENT spelling, a DIFFERENT command —
        // must still be standing in the rebuilt map: the byte-exact writer
        // never touches it, so the simulation may not claim it either.
        assert_eq!(
            w.outcome,
            UnbindOutcome::Runs {
                command: "cursor.top".to_owned(),
                avail: Availability::Here,
            },
            "the twin the writer would leave behind must still be reported as running"
        );

        // End to end: the real writer agrees.
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(
            dir.path().join("keymap.toml"),
            "[pane]\nprepend_keymap = [\n\
                { on = [\"ctrl+p\"], run = \"pane.move\" },\n\
                { on = [\"mod+p\"], run = \"cursor.top\" },\n\
             ]\n",
        )
        .expect("seed");
        norte_config::persist_keymap_unbind(dir.path(), w.section, &w.chords, &w.command)
            .expect("write");
        let cfg = crate::config::load(&Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        })
        .expect("loads");
        let after = Effective::build_for(&preset, &cfg.keymap_layers, KNOWN, Screen::Browse)
            .expect("builds");
        assert!(
            after.single_chord_runs(c("mod+p"), "cursor.top"),
            "the twin survives the real write, exactly as the simulation now predicts"
        );
    }

    /// Removing the user's entry is not the same as the key going quiet: a
    /// project layer can still bind it. The outcome is worded from the
    /// REBUILT map, so the editor says what the key does now instead of what
    /// the file no longer says.
    #[test]
    fn an_unbind_shadowed_by_a_project_layer_says_the_key_still_runs() {
        let preset = parse_keymap(BROWSE).expect("preset");
        let target = crate::keymap::parse_keymap_layer(
            "[pane]\nprepend_keymap = [{ on = [\"ctrl+j\"], run = \"pane.move\" }]\n",
        )
        .expect("the user's own (already shadowed) entry");
        let project = crate::keymap::parse_keymap_layer(
            "[pane]\nprepend_keymap = [{ on = [\"ctrl+j\"], run = \"cursor.top\" }]\n",
        )
        .expect("project layer");
        let src = RebindSources {
            above: std::slice::from_ref(&project),
            ..only_user(&preset, &target, Screen::Browse)
        };
        let seq = [c("ctrl+j")];
        let w = unbind_dry_run(&src, &seq).expect("removing the user's own entry always succeeds");
        // The REAL command the file named, not what the map currently
        // resolves to (the project layer already shadows it) — the whole
        // reason the old byte-exact match, keyed on the row's (shadowed)
        // command, could never find this entry.
        assert_eq!(w.command, "pane.move");
        assert_eq!(
            w.outcome,
            UnbindOutcome::Runs {
                command: "cursor.top".to_owned(),
                avail: Availability::Here,
            },
            "the key keeps running the project's binding"
        );
    }

    /// Nothing to remove is not an error and not a lie: the file did not have
    /// it, and the answer says so.
    #[test]
    fn an_unbind_of_a_key_the_file_does_not_bind_removes_nothing() {
        let preset = parse_keymap(BROWSE).expect("preset");
        let none = KeymapFile::default();
        let seq = [c("ctrl+j")];
        let src = only_user(&preset, &none, Screen::Browse);
        let w = unbind_dry_run(&src, &seq).expect("a no-op is not an error");
        assert_eq!(w.outcome, UnbindOutcome::NotBound);
        assert_eq!(w.command, "", "nothing was found to name a command");
        assert_eq!(
            w.chords,
            vec!["ctrl+j".to_owned()],
            "Display's own spelling: there is no file spelling to prefer"
        );
    }
}
