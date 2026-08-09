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

use norte_config::KeymapList;

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
///   goes into — `alt+ctrl+p` for what `Display` writes as `ctrl+alt+p`, or
///   `mod+p` for `ctrl+p`. Both make a write that loads and never fires — the
///   silent failure this whole module exists to stop — so answering it is
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
/// # Where the three slices come from
///
/// **Not** from `FrontendConfig::keymap_layers`. That is a bare
/// `Vec<KeymapFile>` with one entry per layer dir that HAS a `keymap.toml`,
/// and the [`Layer`](norte_config::Layer) kind is dropped — so the cut cannot
/// be recovered from it, and the two ways of guessing are both wrong in a way
/// nothing would report:
///
/// - taking the last layer as `target` hands over the PROJECT layer when there
///   is one. The write is then modelled above the layer that actually outranks
///   it, and [`RebindError::Shadowed`] — the whole reason for the split —
///   stops firing;
/// - taking the last non-project layer hands over the SYSTEM layer when the
///   user has no file yet, which is the first rebind of every new install. The
///   model puts the write below the user's own layer and refuses writes that
///   would have worked.
///
/// Build it from [`Layers`](norte_config::Layers), which does carry the kinds,
/// re-reading each layer with [`load_keymap_layer`](crate::config::load_keymap_layer):
/// `below` is every dir before the one being written, `target` is that dir's
/// layer or [`KeymapFile::default`] when it has no file, `above` is every dir
/// after it. One extra read of files the writer is about to lock anyway.
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
    /// there is no file yet. Never a PROJECT layer: `./.norte` is not the
    /// editor's to write, and a project layer here would also have its `lua:`
    /// bindings discarded on merge, which the write cannot reproduce.
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
/// as `ctrl+alt+p`, or `mod+p` for `ctrl+p`, both perfectly legal to hand-write
/// — which the byte-exact writer would walk past, leaving two entries for one
/// chord and the OLDER one winning. That is the deliberate difference: the
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

    use super::{Rebind, RebindError, RebindSources, RebindWrite, rebind_check, rebind_dry_run};
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

    /// The K2b case: the key the user wants is bound to something norte has
    /// not built, and the editor has to be able to SAY so — which is the whole
    /// reason `Replaces` carries the availability.
    #[test]
    fn replaces_carries_the_availability_of_what_it_displaces() {
        let v = rebind_check(&eff(BROWSE, Screen::Browse), &[c("alt+f5")]);
        assert_eq!(
            v,
            Rebind::Replaces {
                command: "pane.pack".to_owned(),
                avail: Availability::NotBuilt {
                    reason: "keymap-reason-archive-write",
                    issue: 132
                },
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
}
