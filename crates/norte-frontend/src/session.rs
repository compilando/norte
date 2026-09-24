//! The BODY of the UI session (L2): what the core stores and does not read.
//!
//! The core stores an opaque document — `version`, `revision`, `body` —
//! because [`Node`], [`SortSpec`] and [`ColumnId`] live HERE, and this crate
//! depends on `norte-proto` and not the other way around (ADR 0058). This
//! module is the other half: that body's schema, its version, and the caps
//! that keep a saved screen from growing without end.
//!
//! **The caps are the client's**, and they live in the type, not in the
//! caller: a cap discovered later is a migration, and one that each caller
//! trims its own way is three different caps.

use std::collections::{BTreeMap, BTreeSet};

use norte_proto::VPath;
use serde::{Deserialize, Serialize};

use crate::columns::ColumnId;
use crate::layout::{Node, SlotId};
use crate::sort::SortSpec;

/// The body's schema. Owned by this crate, not by the wire: adding a field to
/// [`SlotState`] bumps THIS number, not the protocol's version.
pub const SCHEMA_VERSION: u32 = 2;

/// History entries per slot and per direction.
pub const HISTORY_CAP: usize = 64;

/// Orphan slots — the ones no layout mentions — that get saved.
pub const ORPHAN_CAP: usize = 128;

/// History entries an ORPHAN slot keeps, per direction.
///
/// A slot no layout mentions is not on screen: nobody can press "back"
/// inside it without reopening it first, and reopening it is starting to
/// walk again. [`HISTORY_CAP`]'s 64 steps are for the slot that is visible.
///
/// The number comes from ARITHMETIC, not taste (#304): [`ORPHAN_CAP`] is 128
/// and a slot with a full history measures ~9,240 bytes with this
/// repository's paths, so 128 orphans on their own gave ~1,182,000 against
/// [`norte_proto::methods::SESSION_BODY_MAX`]'s 1,048,576 — a body the core
/// REFUSES, leaving the session as it was. `prune` trims against counts and
/// the real cap is in bytes; lowering the history nobody is looking at is
/// what gives the count cap back its meaning.
/// `the_orphan_cap_full_also_fits_in_the_envelope` measures both bounds at
/// once; if it goes red, the fix is LOWERING this number.
pub const ORPHAN_HISTORY_CAP: usize = 8;

/// How many PROFILES keep state at once (spec 2026-08-26, D6).
///
/// The number comes from a MEASUREMENT, not taste:
/// `a_realistic_body_with_the_cap_full_fits_in_the_envelope` serializes four
/// profiles of eight slots with the history full in both directions and this
/// repository's paths, and gives **295,567 bytes** against
/// [`norte_proto::methods::SESSION_BODY_MAX`]'s 1,048,576 — 28% of the
/// envelope, with room for someone else's paths to be quite a bit longer
/// than these. If that test goes red, the fix is LOWERING this number: the
/// core refuses a `put` that goes over and leaves the session as it was, so
/// going over means losing what you were doing.
///
/// Past the cap, the state of the profile that has been activated least
/// recently goes, WHOLE. Its config directory is not touched: the profile
/// keeps existing and its next start comes from `[profile.start]`.
pub const PROFILE_STATE_CAP: usize = 4;

/// Suffix of the `layouts` key under which the WINDOW saves its layout (ADR
/// 0139): `default@window`, `<profile>@window`.
///
/// The terminal and the window each remember their own — pane sizes and
/// positions — because sharing it meant the last one to write stomped on
/// what the other had adjusted. The terminal's key stays just the profile's
/// name.
pub const WINDOW_LAYOUT_SUFFIX: &str = "@window";

/// The window's key for the profile keyed `profile`.
#[must_use]
pub fn window_layout_key(profile: &str) -> String {
    format!("{profile}{WINDOW_LAYOUT_SUFFIX}")
}

/// The profile a `layouts` key belongs to: the window's counts as its
/// profile's for pruning, and is pruned with it.
fn profile_of(key: &str) -> &str {
    key.strip_suffix(WINDOW_LAYOUT_SUFFIX).unwrap_or(key)
}

/// Age at which an orphan gets swept: thirty days in milliseconds.
pub const MAX_AGE_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// Why a body could not be read.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The body was written by a newer binary. Refused whole: better to
    /// start from the configuration than to interpret fields that are not
    /// yours.
    #[error("the session is version {version} and this one knows {SCHEMA_VERSION}")]
    FromTheFuture {
        /// The version it carried.
        version: u32,
    },
    /// Does not fit the schema. The message does NOT quote the content: a
    /// session body carries paths, and a path does not go to a log over a
    /// parse error.
    #[error("the session does not fit the schema ({reason})")]
    Malformed {
        /// Category and position, never the value that did not fit.
        reason: String,
    },
    /// The body parses, but one of its layouts cannot be used.
    ///
    /// Kept apart from [`Self::Malformed`] because the cause is different and
    /// so is the fix: here the JSON was fine and what is invalid is the
    /// tree, so whoever wrote it was a version of norte, not a text editor.
    #[error("the session carries an invalid layout: {reason}")]
    BadLayout {
        /// What is wrong with the tree. Never a slot's content.
        reason: String,
    },
}

/// The state of ONE slot: where it is, how it looks and where it has been.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotState {
    /// Where the slot is. `VPath`, never `String`: it is the only field of
    /// this struct that is neither a number nor an enum, and typing it as
    /// text would lose a non-UTF-8 name without any test noticing (rule 1).
    pub path: VPath,
    /// Cursor row within the listing.
    #[serde(default)]
    pub cursor: u64,
    /// Backward history, oldest to most recent.
    #[serde(default)]
    pub back: Vec<VPath>,
    /// Forward history, oldest to most recent.
    #[serde(default)]
    pub forward: Vec<VPath>,
    /// The slot's jump point (`nav.set-jump-point`, spec 2026-09-15 D5).
    /// Additive: an old body reads it empty and does not bump
    /// [`SCHEMA_VERSION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump: Option<VPath>,
    /// Listing order.
    #[serde(default, deserialize_with = "orden::deserialize")]
    pub sort: SortSpec,
    /// Visible columns, in their stable string form (`name`, `attr:…`,
    /// `plugin:…/…`): the SAME as the configuration, and not a second
    /// vocabulary to maintain.
    ///
    /// **Today it ALWAYS travels empty from the TUI (#236)**, where visible
    /// columns are per-scheme configuration and not per-slot state. The
    /// field exists because a frontend that does have them per slot needs
    /// it, and because removing it later would cost bumping
    /// [`SCHEMA_VERSION`]; whoever fills it has to fill it at capture time,
    /// not here.
    #[serde(default, with = "columnas")]
    pub columns: Vec<ColumnId>,
    /// Whether hidden entries are shown.
    #[serde(default)]
    pub show_hidden: bool,
    /// Last touched (epoch ms). Written by the client, like every cap: the
    /// sweep by age needs a clock to sweep against, and the core does not
    /// read this document.
    #[serde(default)]
    pub touched_ms: u64,
    /// What is MARKED in this slot, up to [`MARKS_CAP`] (phase 9).
    ///
    /// Marks are the only thing on screen that did not use to survive a
    /// handoff between frontends, and it is exactly the most expensive thing
    /// to redo: recovering a directory and a cursor is one `cd`; recovering
    /// forty hand-marked files is marking them all over again.
    ///
    /// **They are `VPath`, i.e. the row's IDENTITY, never its index.** A
    /// list that gets reordered or loses a neighbor above leaves an index
    /// pointing at another file, and what would be restored is a selection
    /// nobody made — on which delete then gets pressed. It is the same
    /// reason live marks are saved by path.
    ///
    /// Additive: an old body reads it empty and does not bump
    /// [`SCHEMA_VERSION`], same as `jump` or `palette_recent`. And it is
    /// omitted when empty, which is the normal case: a slot with no marks
    /// produces the SAME bytes as before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marks: Vec<VPath>,
}

/// The saved screen: layouts by name and state by slot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionBody {
    /// The active profile. Empty = none.
    ///
    /// It is STATE, not configuration: what you were doing, not what you
    /// decided. That is why it lives here and not in the reader's
    /// `norte.toml`, which stays a file they wrote themselves.
    ///
    /// It is `String` and not `OsString` because it is the KEY of
    /// [`Self::layouts`], which is a JSON object and therefore UTF-8 by
    /// construction. A profile whose directory is not UTF-8 is fine for
    /// configuration and cannot carry state, sticky either (spec 2026-08-26,
    /// D4).
    #[serde(default)]
    pub active: String,
    /// Layouts by PROFILE name. Empty or `default` is the reader's without a
    /// profile; with [`Self::active`] set, the key is that name.
    #[serde(default)]
    pub layouts: BTreeMap<String, Node>,
    /// State by slot, indexed by [`SlotId`].
    #[serde(default)]
    pub slots: BTreeMap<u32, SlotState>,
    /// The last dispatch keys launched from the palette, most recent first,
    /// up to [`PALETTE_RECENT_CAP`] (spec 2026-09-10). It is STATE, like the
    /// active profile: what you did, not what you decided. A field with
    /// `default` is additive: an old body reads it empty and a new one
    /// writes it; it does not bump [`SCHEMA_VERSION`].
    #[serde(default)]
    pub palette_recent: Vec<String>,
    /// The whole session's popular directories
    /// ([`crate::history::Popular`], spec 2026-09-15 D6), in the order they
    /// are saved. Additive like [`Self::palette_recent`].
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "populares::deserialize"
    )]
    pub popular: Vec<crate::history::PopularEntry>,
}

/// Populars are read ENTRY BY ENTRY.
///
/// A path that does not parse — a hand-edited body — is skipped instead of
/// refusing the whole body, which would take down layouts and slots that
/// have nothing to do with it. It is a list of shortcuts that gets rebuilt
/// by walking around (spec 2026-09-15 D6); a slot's path, on the other hand,
/// is still an error, because without it the slot is nothing.
mod populares {
    use serde::{Deserialize, Deserializer};

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<crate::history::PopularEntry>, D::Error> {
        let raw = Vec::<serde_json::Value>::deserialize(d)?;
        Ok(raw
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect())
    }
}

/// How many recent commands the palette keeps. Five: the ones that fit in
/// view without pushing the whole list under the edge.
pub const PALETTE_RECENT_CAP: usize = 5;

/// Marks a slot keeps (phase 9, spec 2026-09-15).
///
/// The cap lives HERE and not in each caller, for the same reason as the
/// others: a document bounded in five places is bounded in four. Four
/// thousand ninety-six paths from this repository run around 160 KiB —
/// comfortably under [`norte_proto::methods::SESSION_BODY_MAX`]'s 1 MiB —
/// and past that figure what there is is not a selection a human made by
/// hand, but a "mark everything" over a huge directory, which gets redone
/// with one key.
pub const MARKS_CAP: usize = 4096;

/// Notes `key` as the palette's most recent command: puts it first, removes
/// its earlier repeat and trims to [`PALETTE_RECENT_CAP`].
pub fn note_palette_recent(recent: &mut Vec<String>, key: &str) {
    recent.retain(|k| k != key);
    recent.insert(0, key.to_owned());
    recent.truncate(PALETTE_RECENT_CAP);
}

impl SessionBody {
    /// Trims the session to its caps. Called on WRITE, which is where it
    /// grows.
    ///
    /// In this order: the cap on PROFILES with state ([`PROFILE_STATE_CAP`],
    /// spec 2026-08-26, D6) first, so everything else already sees the
    /// smaller map; the slots some layout mentions are marked untouchable;
    /// each slot's history is trimmed from the OLD end — what gets dropped
    /// is the farthest away, not what was just walked — to [`HISTORY_CAP`]
    /// if the slot is visible and to [`ORPHAN_HISTORY_CAP`] if not; and of
    /// the orphans, the ones older than [`MAX_AGE_MS`] go first and then, if
    /// there are still too many, the ones touched least recently until they
    /// fit [`ORPHAN_CAP`].
    ///
    /// A VISIBLE slot is swept by neither age nor cap, and loses not a single
    /// history step: what is on screen is not recycled.
    ///
    /// The [`Self::active`] profile is swept by nothing, at any step.
    ///
    /// And at the end, the cap that really rules: the SERIALIZED body is
    /// measured and trimmed further until it fits
    /// [`norte_proto::methods::SESSION_BODY_MAX`]. All the ones above are by
    /// COUNT and the core's is by BYTES, so no count can promise the body
    /// fits; the order it degrades in is declared in `fit_to_envelope`, and
    /// what is never touched is the active profile, its layout, and each
    /// visible slot's path and cursor.
    pub fn prune(&mut self, now_ms: u64) {
        self.prune_profiles();
        if self.popular.len() > crate::history::POPULAR_CAP {
            // The SAME eviction rule as on a visit, not a `truncate`: the
            // saved order is not the order of importance.
            self.popular = crate::history::Popular::from_entries(std::mem::take(&mut self.popular))
                .entries()
                .to_vec();
        }
        let visible: BTreeSet<u32> = self
            .layouts
            .values()
            .flat_map(Node::slot_ids)
            .map(|SlotId(id)| id)
            .collect();
        for (id, slot) in &mut self.slots {
            let cap = if visible.contains(id) {
                HISTORY_CAP
            } else {
                ORPHAN_HISTORY_CAP
            };
            trim_history(&mut slot.back, cap);
            trim_history(&mut slot.forward, cap);
        }
        self.slots.retain(|id, s| {
            visible.contains(id) || now_ms.saturating_sub(s.touched_ms) <= MAX_AGE_MS
        });
        let mut orphans: Vec<(u64, u32)> = self
            .slots
            .iter()
            .filter(|(id, _)| !visible.contains(*id))
            .map(|(id, s)| (s.touched_ms, *id))
            .collect();
        if orphans.len() > ORPHAN_CAP {
            // By age of contact: the ones at the top go, which are the ones
            // nobody has looked at in the longest.
            orphans.sort_unstable();
            let overflow = orphans.len() - ORPHAN_CAP;
            for (_, id) in orphans.into_iter().take(overflow) {
                self.slots.remove(&id);
            }
        }
        self.fit_to_envelope(&visible);
    }

    /// Trims until the body REALLY fits, measuring bytes.
    ///
    /// All of [`Self::prune`]'s caps are by count and the core's is by bytes
    /// ([`norte_proto::methods::SESSION_BODY_MAX`]), so no count can promise
    /// the body fits: the reader chooses the paths. A non-UTF-8 name travels
    /// percent-encoded and measures triple; a deep tree multiplies every
    /// history entry; and the number of VISIBLE slots has no cap at all —
    /// nothing stops twenty tabs per profile. When the body goes over, the
    /// core refuses the WHOLE `put` and the stored session stays as it was.
    ///
    /// The order it degrades in is the order that hurts least, and it is
    /// declared here because a trim the reader cannot predict is worse than
    /// one they can:
    ///
    /// 1. the orphans, WHOLE, and the one touched least recently going
    ///    forward first — nobody is looking at them;
    /// 2. the populars, whole — they are shortcuts that get rebuilt by
    ///    walking around;
    /// 3. the visible ones' history, halved each round down to zero — steps
    ///    backward are lost, not where you are;
    /// 4. the layouts of profiles that are not the active one, with their
    ///    slots, from the one activated least recently going forward.
    ///
    /// What is never touched: the [`Self::active`] profile, its layout, and
    /// the PATH and cursor of every visible slot. If it still does not fit
    /// even so — a body with a single layout of monstrous paths — whatever
    /// there is gets sent: the core's rejection is honest and the frontend
    /// says so, while making up a trim of the active tree would hand the
    /// reader back a screen they did not leave.
    fn fit_to_envelope(&mut self, visible: &BTreeSet<u32>) {
        if self.fits() {
            return;
        }
        let mut orphans: Vec<(u64, u32)> = self
            .slots
            .iter()
            .filter(|(id, _)| !visible.contains(*id))
            .map(|(id, s)| (s.touched_ms, *id))
            .collect();
        orphans.sort_unstable();
        for (_, id) in orphans {
            self.slots.remove(&id);
            if self.fits() {
                return;
            }
        }
        if !self.popular.is_empty() {
            self.popular.clear();
            if self.fits() {
                return;
            }
        }
        let mut cap = HISTORY_CAP;
        while cap > 0 {
            cap /= 2;
            for slot in self.slots.values_mut() {
                trim_history(&mut slot.back, cap);
                trim_history(&mut slot.forward, cap);
            }
            if self.fits() {
                return;
            }
        }
        // From the one activated least recently going forward, and the
        // ACTIVE one is not on this list: it is the only one that cannot be
        // dropped.
        for name in self.profiles_by_last_touch() {
            let trees = self.remove_profile(&name);
            if trees.is_empty() {
                continue;
            }
            let alive: BTreeSet<u32> = self
                .layouts
                .values()
                .flat_map(Node::slot_ids)
                .map(|SlotId(id)| id)
                .collect();
            for SlotId(id) in trees.iter().flat_map(Node::slot_ids) {
                if !alive.contains(&id) {
                    self.slots.remove(&id);
                }
            }
            if self.fits() {
                return;
            }
        }
    }

    /// Degrades the body to RETRY a `put` the core refused for size, and
    /// says whether there was anything left to drop (#316).
    ///
    /// Drops both of each slot's trails, which is the biggest chunk of a
    /// session and the one that hurts least to lose: steps backward are
    /// lost, not where you are. What is never touched are the paths, the
    /// cursor or the layouts — a refused `put` leaves the STORED session as
    /// it was, so the reader loses their whole screen, and coming back with
    /// an empty history is infinitely better than coming back to where they
    /// were a week ago.
    ///
    /// `false` = there is no history left. Retrying then just asks for the
    /// same error again, and the honest thing is to say it did not save.
    ///
    /// Lives here and not in each frontend because it is a DECISION and not
    /// plumbing: the TUI made it in its writer and the window did not make
    /// it at all — any `session_put` error was "did not arrive", with no
    /// degrading and no warning — which is exactly ADR 0077's silent
    /// divergence. [`Self::prune`]'s trim by bytes makes this almost never
    /// necessary; almost.
    pub fn degrade_for_size(&mut self) -> bool {
        let mut had = false;
        for slot in self.slots.values_mut() {
            had |= !slot.back.is_empty() || !slot.forward.is_empty();
            slot.back.clear();
            slot.forward.clear();
        }
        had
    }

    /// Does this body fit in the envelope the core accepts?
    ///
    /// Measured by serializing, which is the only thing that answers the
    /// real question — the core measures the `body`'s bytes, not its
    /// elements. A failure to serialize counts as fitting: failing to
    /// serialize is a different problem, `put` will see it, and trimming
    /// over it would throw away good state for a reason that is not this
    /// one.
    fn fits(&self) -> bool {
        serde_json::to_vec(&self.to_value())
            .map_or(true, |b| b.len() <= norte_proto::methods::SESSION_BODY_MAX)
    }

    /// The profiles with state that are NOT the active one, from the one
    /// activated least recently to the most recent.
    ///
    /// "Activated least recently" is DERIVED and not stored: it is the
    /// profile whose most recently touched slot was touched before everyone
    /// else's. No new field and no clock — the same discipline as
    /// `SlotStore`'s orphan order, where a clock would make the tests
    /// time-dependent. On a tie of touch, by name: pruning has to be
    /// deterministic and not depend on the map's order.
    fn profiles_by_last_touch(&self) -> Vec<String> {
        let last_touch = |tree: &Node| -> u64 {
            tree.slot_ids()
                .into_iter()
                .filter_map(|SlotId(id)| self.slots.get(&id))
                .map(|s| s.touched_ms)
                .max()
                .unwrap_or(0)
        };
        // By PROFILE, not by key: the window's layout (`<profile>@window`)
        // belongs to the same profile as the terminal's, and its touch
        // counts for both.
        let mut touches: std::collections::BTreeMap<String, u64> =
            std::collections::BTreeMap::new();
        for (key, tree) in &self.layouts {
            let profile = profile_of(key);
            if profile == self.active {
                continue;
            }
            let t = touches.entry(profile.to_owned()).or_insert(0);
            *t = (*t).max(last_touch(tree));
        }
        let mut order: Vec<(u64, String)> = touches.into_iter().map(|(p, t)| (t, p)).collect();
        order.sort_unstable();
        order.into_iter().map(|(_, name)| name).collect()
    }

    /// Removes the layouts of profile `profile` — the terminal's and the
    /// window's — and returns them.
    fn remove_profile(&mut self, profile: &str) -> Vec<Node> {
        let keys: Vec<String> = self
            .layouts
            .keys()
            .filter(|c| profile_of(c) == profile)
            .cloned()
            .collect();
        keys.iter().filter_map(|c| self.layouts.remove(c)).collect()
    }

    /// How many PROFILES have state (the window does not count separately).
    fn profiles_with_state(&self) -> usize {
        self.layouts
            .keys()
            .map(|c| profile_of(c))
            .collect::<BTreeSet<_>>()
            .len()
    }

    /// Leaves at most [`PROFILE_STATE_CAP`] profiles with state, dropping
    /// whole the ones activated least recently.
    ///
    /// "Activated least recently" is DERIVED and not stored: it is the
    /// profile whose most recently touched slot was touched before everyone
    /// else's. No new field and no clock — the same discipline as
    /// `SlotStore`'s orphan order, where a clock would make the tests
    /// time-dependent.
    fn prune_profiles(&mut self) {
        let profiles = self.profiles_with_state();
        if profiles <= PROFILE_STATE_CAP {
            return;
        }
        let order = self.profiles_by_last_touch();
        let overflow = profiles - PROFILE_STATE_CAP;
        let mut candidates: BTreeSet<u32> = BTreeSet::new();
        for name in order.into_iter().take(overflow) {
            for tree in self.remove_profile(&name) {
                candidates.extend(tree.slot_ids().into_iter().map(|SlotId(id)| id));
            }
        }
        // The departing profile's slots are erased against WHAT IS LEFT, not
        // blindly by its tree.
        //
        // That two profiles do not share a slot is an invariant of the
        // ALLOCATION (`next_slot_base` + `rebase_slot_ids`), and nothing
        // enforces it on a body that arrives from disk: `from_value`
        // validates each tree separately — duplicates WITHIN one — and says
        // nothing about an id shared between TWO, and the body is opaque to
        // the core, so any client can write one like that. Erasing blindly,
        // a body like that would take down the ACTIVE profile's slots along
        // with it: the reader would lose the directory, the cursor and both
        // trails of the panes they were looking at, which is exactly what
        // `prune`'s rustdoc promises does not happen.
        //
        // And only the outgoing profile's ids are looked at: other
        // profiles' orphans are NOT touched here, that is what the sweep by
        // age below is for.
        let alive: BTreeSet<u32> = self
            .layouts
            .values()
            .flat_map(Node::slot_ids)
            .map(|SlotId(id)| id)
            .collect();
        for id in candidates {
            if !alive.contains(&id) {
                self.slots.remove(&id);
            }
        }
    }

    /// The first slot id NOBODY uses: not a layout of any profile, not a
    /// saved state, orphans included.
    ///
    /// It is the base [`Node::rebase_slot_ids`] needs so two profiles do not
    /// share a slot (spec 2026-08-26, D5). Looks at EVERYTHING and not just
    /// the active profile on purpose: allocating against what is on screen
    /// would end up reassigning over another profile's saved state, which is
    /// exactly the state nobody is looking at when it happens.
    ///
    /// An empty session starts at 1. `None` = no room left: the highest id
    /// in use is `u32::MAX`, and there is no "next one". Returning it
    /// saturated would say `u32::MAX` is free while it is occupied, with the
    /// result that [`Node::rebase_slot_ids`] would hand out that same number
    /// to every slot of the tree.
    #[must_use]
    pub fn next_slot_base(&self) -> Option<u32> {
        let from_trees = self
            .layouts
            .values()
            .flat_map(Node::slot_ids)
            .map(|SlotId(id)| id);
        let from_state = self.slots.keys().copied();
        match from_trees.chain(from_state).max() {
            None => Some(1),
            Some(m) => m.checked_add(1),
        }
    }

    /// The body as a JSON document.
    ///
    /// **No `version` inside** since #247: the body's schema is declared by
    /// [`norte_proto::methods::SessionPutParams::version`], which is the
    /// field the protocol documents and the only one the core looks at.
    /// There used to be TWO, and nobody read the documented one — an
    /// unrelated client doing what the contract says (putting a v2 body and
    /// `version: 2` in the envelope) reached a reader that only looked at
    /// the copy inside, saw it absent, took it for 0, and ate the fields it
    /// did not understand.
    ///
    /// A body written by an earlier version DOES carry the copy, and
    /// [`Self::from_value`] still reads it: removing it from here cannot
    /// invalidate what is already on disk.
    ///
    /// # Panics
    ///
    /// Never: the struct is made of types that always serialize, and the
    /// only map with a non-string key has a numeric one.
    #[must_use]
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("SessionBody always serializes")
    }

    /// Reads a body, checking the version BEFORE the shape.
    ///
    /// `envelope` is the version the ENVELOPE declares
    /// ([`norte_proto::methods::Session::version`]), the one the protocol
    /// documents. Takes the HIGHER of the two — the envelope and the copy
    /// old bodies carry inside — because both are a claim about who wrote
    /// it, and refusing is the safe thing: reading a body newer than what is
    /// understood and writing it back again silently drops fields, which is
    /// what ADR 0059 promises does not happen (#247).
    ///
    /// # Errors
    ///
    /// [`SessionError::FromTheFuture`] if a newer binary wrote it,
    /// [`SessionError::Malformed`] if it does not fit the schema and
    /// [`SessionError::BadLayout`] if it carries an unusable layout.
    pub fn from_value(envelope: u32, v: &serde_json::Value) -> Result<Self, SessionError> {
        // The version first: refusing a body from the future cannot depend
        // on its shape fitting this binary.
        let inside = v
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let version = inside.max(u64::from(envelope));
        if version > u64::from(SCHEMA_VERSION) {
            return Err(SessionError::FromTheFuture {
                version: u32::try_from(version).unwrap_or(u32::MAX),
            });
        }
        let body: Self =
            serde_json::from_value(v.clone()).map_err(|e| SessionError::Malformed {
                reason: diagnose(&e),
            })?;
        // A layout with no listing PARSES — the schema does not forbid it —
        // and it panicked on being applied, on every start while the
        // session file stayed there (#242). The whole body is rejected: the
        // user starts from their configuration, which is repairable,
        // instead of from a screen that is not.
        for (name, tree) in &body.layouts {
            crate::layout::validate(tree).map_err(|e| SessionError::BadLayout {
                reason: format!("{name}: {e}"),
            })?;
        }
        Ok(body)
    }
}

/// What to do with the session on THIS tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushStep {
    /// Nothing: either nothing has changed, or something in front says now
    /// is not the moment to save where you are.
    Skip,
    /// Ask whether this window can write yet. Only a LONE window asks it,
    /// every `retry_every` ticks.
    Ask,
    /// Capture the screen and, if [`PushPolicy::prepare`] says it changed,
    /// send it.
    Capture,
}

/// The session's write policy: when it is sent, when it is not repeated,
/// when ownership is asked for again, and what gets trimmed before sending.
///
/// **Lives here and not in the frontend (#236).** The first client of
/// `session.put` was the TUI, and this whole policy was born inside its
/// event loop, trimming state included; the second frontend would have
/// reimplemented it whole, trimming bugs included. What is NOT here is the
/// plumbing: the channels, the clock and the `put` belong to whoever has a
/// runtime.
#[derive(Debug)]
pub struct PushPolicy {
    /// The last thing SENT to write: compared so the same thing is not sent
    /// twice. Comparing the whole document costs less than a dirty flag set
    /// by hand in the hundreds of places that move a cursor — and cannot be
    /// forgotten in one of them.
    last: Option<std::sync::Arc<SessionBody>>,
    /// Ticks left before asking about ownership again.
    retry_in: u32,
    /// Every how many ticks a lone window asks.
    retry_every: u32,
}

impl PushPolicy {
    /// A policy that asks about ownership every `retry_every` ticks.
    ///
    /// `retry_every` at 0 is treated as 1: asking "every zero ticks" is not
    /// a cadence, and the alternative — never asking — is bug #234 all over
    /// again.
    #[must_use]
    pub fn new(retry_every: u32) -> Self {
        let retry_every = retry_every.max(1);
        Self {
            last: None,
            retry_in: retry_every,
            retry_every,
        }
    }

    /// What is due this tick.
    ///
    /// `detached`: this window is not the owner, so it does not write — but
    /// it does ask again, because the owner may have closed a while ago and
    /// nobody warns about that (#234). `blocked`: something is in front (a
    /// modal) and the session is about where you are, not what you are
    /// deciding.
    pub fn tick(&mut self, detached: bool, blocked: bool) -> PushStep {
        if detached {
            self.retry_in = self.retry_in.saturating_sub(1);
            if self.retry_in == 0 {
                self.retry_in = self.retry_every;
                return PushStep::Ask;
            }
            return PushStep::Skip;
        }
        if blocked {
            return PushStep::Skip;
        }
        PushStep::Capture
    }

    /// Trims the body to its caps and, if it has changed since the last
    /// thing sent, seals the LIVE slots that moved.
    ///
    /// `None` is "nothing changed": this tick sends nothing. `Some(sealed)`
    /// are the slots the caller also has to seal in its own state, with this
    /// same `now_ms` — the seal cannot come out of the capture because it
    /// needs to know what to compare against.
    ///
    /// And only the live ones: sealing the orphans too would give them back
    /// their youth on every start and the sweep by age would never sweep.
    pub fn prepare(
        &self,
        body: &mut SessionBody,
        live: &[SlotId],
        now_ms: u64,
    ) -> Option<Vec<SlotId>> {
        body.prune(now_ms);
        if self.last.as_deref() == Some(&*body) {
            return None;
        }
        let mut sealed = Vec::new();
        for id in live {
            let Some(state) = body.slots.get_mut(&id.0) else {
                continue;
            };
            if self
                .last
                .as_deref()
                .and_then(|b| b.slots.get(&id.0))
                .is_some_and(|before| before == state)
            {
                continue;
            }
            state.touched_ms = now_ms;
            sealed.push(*id);
        }
        Some(sealed)
    }

    /// The body was really sent. Only then does it count as written: taking
    /// it as sent when the channel was full loses that body for good.
    pub fn sent(&mut self, body: std::sync::Arc<SessionBody>) {
        self.last = Some(body);
    }

    /// What was sent did NOT arrive (another window wrote first): the
    /// comparison must not take it as written.
    pub fn resend(&mut self) {
        self.last = None;
    }

    /// Ask about ownership on the VERY NEXT tick and not within the whole
    /// cadence: after a daemon handoff the session is usually already free.
    pub fn ask_soon(&mut self) {
        self.retry_in = 1;
    }
}

/// serde's error WITHOUT its message: its `Display` quotes the value that
/// did not fit, and that value comes from a document carrying paths.
fn diagnose(e: &serde_json::Error) -> String {
    let what = match e.classify() {
        serde_json::error::Category::Io => "i/o",
        serde_json::error::Category::Syntax => "malformed JSON",
        serde_json::error::Category::Data => "unexpected shape",
        serde_json::error::Category::Eof => "ends too soon",
    };
    format!("{what} at line {} column {}", e.line(), e.column())
}

/// Keeps the `cap` most RECENT entries, which are the ones at the end.
fn trim_history(h: &mut Vec<VPath>, cap: usize) {
    if h.len() > cap {
        h.drain(..h.len() - cap);
    }
}

/// Columns travel by their stable string form, the same as the config:
/// [`ColumnId`] deliberately has no serde of its own — its canonical form is
/// `Display`/`FromStr`, with a pinned round-trip — and giving it a second one
/// here would be a second vocabulary to maintain.
mod columnas {
    use serde::{Deserialize as _, Deserializer, Serializer};

    use crate::columns::ColumnId;

    /// Each column as its canonical string.
    pub(super) fn serialize<S: Serializer>(v: &[ColumnId], s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(v.iter().map(ToString::to_string))
    }

    /// A column this binary cannot read is DROPPED, it does not break the
    /// whole body: it is one fewer column in one pane, and the rest of the
    /// screen — paths, history, layout — is just as valid.
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<ColumnId>, D::Error> {
        let raw = Vec::<String>::deserialize(d)?;
        Ok(raw.iter().filter_map(|s| s.parse().ok()).collect())
    }
}

/// The order is read TOLERANTLY, for the same reason as the columns: a sort
/// column this binary does not know — `extension`, whenever it gets added —
/// is one pane's preference, and making it fatal would take down the WHOLE
/// screen (layout, paths and history of every slot) over it. Without this,
/// adding a variant to [`crate::sort::SortColumn`] would be a
/// [`SCHEMA_VERSION`] change, which is exactly what this schema says it does
/// not cost.
mod orden {
    use serde::{Deserialize as _, Deserializer};

    use crate::sort::SortSpec;

    /// An order that is not understood is the default order, not a broken
    /// body.
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SortSpec, D::Error> {
        let raw = serde_json::Value::deserialize(d)?;
        Ok(serde_json::from_value(raw).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::Segment;

    use crate::layout::KindId;

    fn vp(s: &str) -> VPath {
        VPath::parse(s).expect("vpath")
    }

    fn slot(path: &str) -> SlotState {
        SlotState {
            path: vp(path),
            cursor: 0,
            back: Vec::new(),
            forward: Vec::new(),
            jump: None,
            sort: SortSpec::default(),
            columns: Vec::new(),
            show_hidden: false,
            touched_ms: 0,
            marks: Vec::new(),
        }
    }

    /// A body with a profile per name, each with two of its own slots and
    /// its `touched_ms`, which is what orders "activated least recently".
    fn body_with_profiles(profiles: &[(&str, u64)]) -> SessionBody {
        let factory = crate::layout::Node::split(
            crate::layout::Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let mut b = SessionBody::default();
        for (name, touched) in profiles {
            let (tree, _) = factory.rebase_slot_ids(b.next_slot_base().expect("there is room"));
            for SlotId(id) in tree.slot_ids() {
                let mut s = slot("file:///casa");
                s.touched_ms = *touched;
                b.slots.insert(id, s);
            }
            b.layouts.insert((*name).to_owned(), tree);
        }
        b
    }

    /// MARKS (phase 9) are additive: a body without them reads the same as
    /// before, and one with them returns them by PATH.
    ///
    /// Truly additive means two things, and both are checked: an old
    /// document — that does not have the field — still reads without
    /// bumping [`SCHEMA_VERSION`], and a slot with no marks produces the
    /// SAME bytes it produced before the field existed. Without the second,
    /// every tick would write a body different from the last one and
    /// coalescing would stop coalescing.
    #[test]
    fn marks_are_additive_and_travel_by_path() {
        let mut s = slot("mem:///casa");
        assert_eq!(
            serde_json::to_value(&s).expect("json").get("marks"),
            None,
            "a slot with no marks does not write the field"
        );
        // And a document that does not carry it is read, which is all that
        // had been saved up to this version.
        let old = serde_json::json!({"path": "mem:///casa"});
        let read: SlotState = serde_json::from_value(old).expect("an old body reads");
        assert!(read.marks.is_empty());

        s.marks = vec![vp("mem:///casa/a.txt"), vp("mem:///casa/b.txt")];
        let out = serde_json::to_value(&s).expect("json");
        let back: SlotState = serde_json::from_value(out).expect("json");
        assert_eq!(back.marks, s.marks, "the PATHS come back, not some indices");
    }

    /// Past the cap, the state of the profile activated least recently goes
    /// WHOLE. Its config directory is not touched: the profile keeps
    /// existing and starts from its `[profile.start]`.
    #[test]
    fn past_the_cap_the_oldest_profile_goes() {
        let mut b = body_with_profiles(&[("a", 10), ("b", 20), ("c", 30), ("d", 40), ("e", 50)]);
        b.active = "e".to_owned();
        b.prune(100);
        assert!(!b.layouts.contains_key("a"), "the oldest one goes");
        assert_eq!(b.layouts.len(), PROFILE_STATE_CAP);
    }

    /// ADR 0139: the window's layout (`<profile>@window`) belongs to the
    /// same profile as the terminal's: it does not count as one more
    /// profile, and it goes and stays with it.
    #[test]
    fn the_windows_layout_goes_with_its_profile() {
        let mut b = body_with_profiles(&[("a", 10), ("b", 20), ("c", 30), ("d", 40)]);
        for p in ["a", "d"] {
            let tree = b.layouts[p].clone();
            b.layouts.insert(window_layout_key(p), tree);
        }
        b.active = "d".to_owned();
        b.prune(100);
        assert_eq!(
            b.layouts.len(),
            6,
            "four profiles, not six: nothing to prune"
        );
        let mut b = body_with_profiles(&[("a", 10), ("b", 20), ("c", 30), ("d", 40), ("e", 50)]);
        for p in ["a", "e"] {
            let tree = b.layouts[p].clone();
            b.layouts.insert(window_layout_key(p), tree);
        }
        b.active = "e".to_owned();
        b.prune(100);
        assert!(!b.layouts.contains_key("a"));
        assert!(!b.layouts.contains_key("a@window"), "goes with its profile");
        assert!(b.layouts.contains_key("e@window"), "the active one's stays");
    }

    /// The ACTIVE one is swept by nothing, at any step, not even being the
    /// oldest.
    #[test]
    fn the_active_one_is_not_swept_even_if_oldest() {
        let mut b = body_with_profiles(&[("a", 10), ("b", 20), ("c", 30), ("d", 40), ("e", 50)]);
        b.active = "a".to_owned();
        b.prune(100);
        assert!(b.layouts.contains_key("a"), "the active one stays");
        assert_eq!(b.layouts.len(), PROFILE_STATE_CAP);
    }

    /// And dropping a profile takes ITS slots, not another one's.
    #[test]
    fn dropping_a_profile_only_takes_its_own_slots() {
        let mut b = body_with_profiles(&[("a", 10), ("b", 20), ("c", 30), ("d", 40), ("e", 50)]);
        b.active = "e".to_owned();
        let of_a: Vec<u32> = b.layouts["a"].slot_ids().iter().map(|s| s.0).collect();
        let of_b: Vec<u32> = b.layouts["b"].slot_ids().iter().map(|s| s.0).collect();
        b.prune(100);
        for id in of_a {
            assert!(
                !b.slots.contains_key(&id),
                "slot {id} of \"a\" went with it"
            );
        }
        for id in of_b {
            assert!(
                b.slots.contains_key(&id),
                "slot {id} of \"b\" is still there"
            );
        }
    }

    /// A body that arrives from DISK can share ids between two profiles: the
    /// disjointness is an invariant of the allocation, and `from_value` only
    /// validates each tree separately. Dropping a profile cannot take down
    /// the ACTIVE one's slots, which is what `prune`'s rustdoc promises.
    #[test]
    fn dropping_a_profile_does_not_touch_slots_another_still_mentions() {
        let shared = crate::layout::Node::split(
            crate::layout::Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let mut b = SessionBody::default();
        // Five profiles with the SAME ids: nothing in the schema forbids it.
        for (i, name) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            b.layouts.insert((*name).to_owned(), shared.clone());
            for SlotId(id) in shared.slot_ids() {
                let mut s = slot("file:///casa");
                s.touched_ms = (i as u64 + 1) * 10;
                b.slots.insert(id, s);
            }
        }
        b.active = "e".to_owned();
        b.prune(100);

        assert_eq!(b.layouts.len(), PROFILE_STATE_CAP, "one is over and goes");
        for SlotId(id) in b.layouts[&b.active].slot_ids() {
            assert!(
                b.slots.contains_key(&id),
                "slot {id} is still shown by the active profile"
            );
        }
    }

    /// With no room left, `next_slot_base` SAYS so instead of answering an id
    /// that is in use, and `rebase_slot_ids` returns the tree intact instead
    /// of handing out the same number to all its slots — which would forge
    /// duplicates out of a healthy tree and make the next `from_value`
    /// refuse the WHOLE body.
    #[test]
    fn with_no_room_left_nothing_is_allocated() {
        let mut b = SessionBody::default();
        b.slots.insert(u32::MAX, slot("file:///casa"));
        assert_eq!(b.next_slot_base(), None, "there is no \"next one\"");

        let tree = crate::layout::Node::split(
            crate::layout::Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let (new, map) = tree.rebase_slot_ids(u32::MAX);
        assert_eq!(new, tree, "the tree comes back as-is");
        assert!(map.is_empty());
        assert!(
            new.duplicate_slot_ids().is_empty(),
            "and above all: no forged duplicates"
        );
    }

    /// A realistic body with the cap full fits in `SESSION_BODY_MAX`. If
    /// this test goes red, the fix is LOWERING [`PROFILE_STATE_CAP`], not
    /// raising the protocol's cap: the core refuses a `put` that goes over
    /// and leaves the session as it was, so going over means losing what
    /// you were doing.
    /// The root of the envelope tests' paths: from this same repository,
    /// because a hand-filled path would measure the filler and not the
    /// case.
    const RAIZ: &str = "file:///home/u/src/norte/crates/norte-frontend/src";

    /// A slot with the history full in both directions.
    fn slot_with_full_history(id: u32) -> SlotState {
        let mut s = slot(&format!("{RAIZ}/modulo{id}"));
        s.back = (0..HISTORY_CAP)
            .map(|i| vp(&format!("{RAIZ}/modulo{id}/atras{i}")))
            .collect();
        s.forward = (0..HISTORY_CAP)
            .map(|i| vp(&format!("{RAIZ}/modulo{id}/alante{i}")))
            .collect();
        s
    }

    /// [`PROFILE_STATE_CAP`] profiles of eight slots, all with the history
    /// full: the VISIBLE body at the cap, without a single orphan.
    fn visible_body_at_cap() -> SessionBody {
        let mut b = SessionBody::default();
        for p in 0..PROFILE_STATE_CAP {
            let children: Vec<Node> = (1..=8u32)
                .map(|i| Node::slot(SlotId(i), KindId::browser()))
                .collect();
            let tree = crate::layout::Node::split(crate::layout::Dir::Horizontal, children);
            let (tree, _) = tree.rebase_slot_ids(b.next_slot_base().expect("there is room"));
            for SlotId(id) in tree.slot_ids() {
                b.slots.insert(id, slot_with_full_history(id));
            }
            b.layouts.insert(format!("perfil{p}"), tree);
        }
        b.active = "perfil0".to_owned();
        b
    }

    #[test]
    fn a_realistic_body_with_the_cap_full_fits_in_the_envelope() {
        let mut b = visible_body_at_cap();
        b.prune(0);

        let bytes = serde_json::to_vec(&b.to_value()).expect("serializes");
        assert!(
            bytes.len() <= norte_proto::methods::SESSION_BODY_MAX,
            "{} bytes against a cap of {}",
            bytes.len(),
            norte_proto::methods::SESSION_BODY_MAX
        );
    }

    /// **And a body that goes over while FITTING every count also fits** in
    /// the end: the real cap is in bytes and no count can promise it
    /// (review of #304).
    ///
    /// Here there is not a single orphan and the four profiles are what the
    /// count allows; what goes over is the number of VISIBLE slots, which
    /// has no cap at all — nothing stops twenty tabs per profile. Without
    /// the trim by bytes, the core refused the WHOLE `put`.
    ///
    /// What CANNOT be lost is checked separately: the active profile, its
    /// layout and the PATH of each of its slots. What is paid for is
    /// history steps, which is the order declared in `fit_to_envelope`.
    #[test]
    fn a_body_that_fits_the_counts_but_not_the_bytes_is_trimmed_all_the_same() {
        let mut b = SessionBody::default();
        for p in 0..PROFILE_STATE_CAP {
            let children: Vec<Node> = (1..=40u32)
                .map(|i| Node::slot(SlotId(i), KindId::browser()))
                .collect();
            let tree = crate::layout::Node::split(crate::layout::Dir::Horizontal, children);
            let (tree, _) = tree.rebase_slot_ids(b.next_slot_base().expect("there is room"));
            for SlotId(id) in tree.slot_ids() {
                b.slots.insert(id, slot_with_full_history(id));
            }
            b.layouts.insert(format!("perfil{p}"), tree);
        }
        b.active = "perfil0".to_owned();
        let active = b.layouts["perfil0"].clone();
        let active_paths: Vec<(u32, VPath)> = active
            .slot_ids()
            .into_iter()
            .map(|SlotId(id)| (id, b.slots[&id].path.clone()))
            .collect();

        b.prune(0);

        let bytes = serde_json::to_vec(&b.to_value()).expect("serializes");
        assert!(
            bytes.len() <= norte_proto::methods::SESSION_BODY_MAX,
            "{} bytes against a cap of {}",
            bytes.len(),
            norte_proto::methods::SESSION_BODY_MAX
        );
        assert_eq!(
            b.layouts.get("perfil0"),
            Some(&active),
            "the whole active one"
        );
        for (id, path) in active_paths {
            assert_eq!(
                b.slots.get(&id).map(|s| &s.path),
                Some(&path),
                "slot {id} of the active profile keeps WHERE it is"
            );
        }
    }

    /// Degrading drops the history and NOTHING else: the paths, the cursor
    /// and the layouts stay, which is what had to be saved (#316).
    ///
    /// And it says whether there was anything to drop, because retrying
    /// without having degraded just asks for the same error again.
    #[test]
    fn degrading_drops_the_history_and_only_the_history() {
        let mut b = visible_body_at_cap();
        let before = b.layouts.clone();
        let paths: BTreeMap<u32, VPath> = b
            .slots
            .iter()
            .map(|(id, s)| (*id, s.path.clone()))
            .collect();

        assert!(b.degrade_for_size(), "there was history to drop");
        assert!(
            b.slots
                .values()
                .all(|s| s.back.is_empty() && s.forward.is_empty()),
            "not a single step is left"
        );
        assert_eq!(b.layouts, before, "the layouts are not touched");
        for (id, path) in paths {
            assert_eq!(b.slots[&id].path, path, "slot {id} is still where it was");
        }
        assert!(
            !b.degrade_for_size(),
            "and the second time nothing is left: retrying would be the same error"
        );
    }

    /// And the ORPHAN cap full also fits, which is not what used to happen
    /// (#304): [`ORPHAN_CAP`] slots nobody looks at, each with the history
    /// full, on top of the visible body at the cap. With [`HISTORY_CAP`] for
    /// all of them this gave ~1,182,000 bytes against the envelope's
    /// 1,048,576, and the `put` was refused WHOLE: the trim that existed to
    /// prevent it was causing it.
    #[test]
    fn the_orphan_cap_full_also_fits_in_the_envelope() {
        let mut b = visible_body_at_cap();
        let base = b.next_slot_base().expect("there is room");
        for k in 0..u32::try_from(ORPHAN_CAP).expect("fits") {
            let id = base + k;
            b.slots.insert(id, slot_with_full_history(id));
        }
        b.prune(0);

        assert_eq!(
            b.slots.len(),
            8 * PROFILE_STATE_CAP + ORPHAN_CAP,
            "not one has been swept: the cap fills up, it does not go over"
        );
        let bytes = serde_json::to_vec(&b.to_value()).expect("serializes");
        assert!(
            bytes.len() <= norte_proto::methods::SESSION_BODY_MAX,
            "{} bytes against a cap of {}",
            bytes.len(),
            norte_proto::methods::SESSION_BODY_MAX
        );
    }

    /// The base comes from EVERYTHING there is: each profile's layouts and
    /// the saved slots, orphans included. Looking only at the active profile
    /// would reassign over another one's state.
    #[test]
    fn the_base_leaves_behind_everything_that_already_exists() {
        let mut b = SessionBody::default();
        b.layouts
            .insert("work".into(), Node::slot(SlotId(4), KindId::browser()));
        b.slots.insert(9, slot("file:///tmp"));
        assert_eq!(b.next_slot_base(), Some(10));
    }

    #[test]
    fn an_empty_session_starts_at_one() {
        assert_eq!(SessionBody::default().next_slot_base(), Some(1));
    }

    /// Two profiles adopted over the SAME factory layout end up with
    /// disjoint slot sets. This is the test that pins the design.
    #[test]
    fn two_profiles_over_the_same_layout_do_not_share_a_slot() {
        let factory = crate::layout::Node::split(
            crate::layout::Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let mut b = SessionBody::default();

        let (t1, _) = factory.rebase_slot_ids(b.next_slot_base().expect("there is room"));
        b.layouts.insert("work".into(), t1);
        let (t2, _) = factory.rebase_slot_ids(b.next_slot_base().expect("there is room"));
        b.layouts.insert("photos".into(), t2);

        let a: BTreeSet<SlotId> = b.layouts["work"].slot_ids().into_iter().collect();
        let c: BTreeSet<SlotId> = b.layouts["photos"].slot_ids().into_iter().collect();
        assert!(a.is_disjoint(&c), "work {a:?} and photos {c:?} overlap");
    }

    /// A layout with no listing PARSES, and applying it left the TUI with no
    /// pane to point at: a panic in raw mode, on every start, until the
    /// session file was deleted by hand (#242). It is rejected on read.
    #[test]
    fn a_session_with_a_layout_with_no_listing_does_not_read() {
        let no_listing = crate::layout::Node::split(
            crate::layout::Dir::Horizontal,
            vec![
                crate::layout::Node::slot(crate::layout::SlotId(1), KindId::new("places")),
                crate::layout::Node::slot(crate::layout::SlotId(4), KindId::new("status")),
            ],
        );
        let body = SessionBody {
            active: String::new(),
            layouts: std::iter::once(("default".to_owned(), no_listing)).collect(),
            slots: std::collections::BTreeMap::new(),
            palette_recent: Vec::new(),
            popular: Vec::new(),
        };
        let v = body.to_value();
        assert!(matches!(
            SessionBody::from_value(SCHEMA_VERSION, &v),
            Err(SessionError::BadLayout { .. })
        ));
    }

    /// #236: a LONE window's cadence belongs to the policy.
    ///
    /// A lone window never writes, but it asks every `retry_every` ticks:
    /// the owner may have closed a while ago and nobody warns about that
    /// (#234).
    #[test]
    fn a_lone_window_asks_on_a_cadence_and_does_not_write() {
        let mut p = PushPolicy::new(3);
        assert_eq!(p.tick(true, false), PushStep::Skip);
        assert_eq!(p.tick(true, false), PushStep::Skip);
        assert_eq!(p.tick(true, false), PushStep::Ask, "asks on the third");
        assert_eq!(
            p.tick(true, false),
            PushStep::Skip,
            "and starts counting again"
        );

        // After a daemon handoff the session is usually already free: it is
        // asked about on the very next tick, not within the whole cadence.
        p.ask_soon();
        assert_eq!(p.tick(true, false), PushStep::Ask);
    }

    /// With a modal in front, nothing is saved: the session is about where
    /// you are, not what you are deciding. And with nothing in front, it is
    /// time to capture.
    #[test]
    fn a_modal_blocks_the_write_and_nothing_else_lets_it_through() {
        let mut p = PushPolicy::new(3);
        assert_eq!(p.tick(false, true), PushStep::Skip);
        assert_eq!(p.tick(false, false), PushStep::Capture);
    }

    /// Sealing belongs to the policy, and it seals ONLY the live slots that
    /// changed: sealing an orphan gives it back its youth on every start and
    /// the sweep by age never sweeps.
    #[test]
    fn only_the_live_ones_that_changed_get_sealed_and_nobody_else() {
        let mut p = PushPolicy::new(3);
        let mut body = SessionBody::default();
        body.slots.insert(1, slot("file:///uno"));
        body.slots.insert(2, slot("file:///dos"));
        // 9 is an orphan: no layout mentions it and it is not in `live`. It
        // is born with an old seal so the sweep does not take it.
        let mut old = slot("file:///nueve");
        old.touched_ms = 1_000;
        body.slots.insert(9, old);

        let live = [SlotId(1), SlotId(2)];
        let sealed = p.prepare(&mut body, &live, 5_000).expect("it is the first");
        assert_eq!(sealed, vec![SlotId(1), SlotId(2)]);
        assert_eq!(body.slots[&1].touched_ms, 5_000);
        assert_eq!(
            body.slots[&9].touched_ms, 1_000,
            "the orphan does not get younger"
        );

        // Sent. The same body again is not sent twice.
        p.sent(std::sync::Arc::new(body.clone()));
        let mut same = body.clone();
        assert!(
            p.prepare(&mut same, &live, 6_000).is_none(),
            "the same thing is not repeated"
        );
        assert_eq!(same.slots[&1].touched_ms, 5_000, "not even re-sealed");

        // Move ONE: only that one gets sealed, not the other.
        let mut moved = body.clone();
        moved.slots.get_mut(&2).expect("the two").cursor = 7;
        let sealed = p.prepare(&mut moved, &live, 7_000).expect("it changed");
        assert_eq!(sealed, vec![SlotId(2)]);
        assert_eq!(
            moved.slots[&1].touched_ms, 5_000,
            "the still one is not touched"
        );
    }

    /// What did not arrive gets sent again: if `resend` did not clear the
    /// last one, the comparison would take as written a body another window
    /// stomped on.
    #[test]
    fn what_did_not_arrive_gets_sent_again() {
        let mut p = PushPolicy::new(3);
        let mut body = SessionBody::default();
        body.slots.insert(1, slot("file:///uno"));
        p.prepare(&mut body, &[SlotId(1)], 5_000).expect("first");
        p.sent(std::sync::Arc::new(body.clone()));
        assert!(p.prepare(&mut body.clone(), &[SlotId(1)], 6_000).is_none());

        p.resend();
        assert!(
            p.prepare(&mut body, &[SlotId(1)], 6_000).is_some(),
            "after a conflict it gets sent again"
        );
    }

    /// A cadence of zero is not a cadence: it is treated as 1. The other way
    /// would be never asking, which is #234 all over again.
    #[test]
    fn a_cadence_of_zero_asks_every_tick() {
        let mut p = PushPolicy::new(0);
        assert_eq!(p.tick(true, false), PushStep::Ask);
        assert_eq!(p.tick(true, false), PushStep::Ask);
    }

    /// Round trip through JSON: what comes out is what went in.
    #[test]
    fn round_trip_through_json() {
        let mut b = SessionBody::default();
        let mut s = slot("file:///casa");
        s.columns = vec![
            "name".parse().expect("name"),
            "attr:posix.mode".parse().expect("attr"),
        ];
        s.cursor = 12;
        s.show_hidden = true;
        b.slots.insert(1, s);
        let back = SessionBody::from_value(SCHEMA_VERSION, &b.to_value()).expect("parses");
        assert_eq!(back, b);
    }

    /// **The proof rule 1 asks for**: a name that is not UTF-8 survives
    /// whole. This is the exact spot where a `String` would have eaten it.
    #[test]
    fn a_non_utf8_name_survives_the_trip() {
        for name in norte_testkit::corpus::hostile_names() {
            let seg = Segment::new(name.bytes.clone()).expect("segment");
            let path = vp("file:///casa").join(seg);
            let mut b = SessionBody::default();
            b.slots.insert(
                1,
                SlotState {
                    path: path.clone(),
                    ..slot("file:///casa")
                },
            );
            let back = SessionBody::from_value(SCHEMA_VERSION, &b.to_value()).expect("parses");
            assert_eq!(
                back.slots[&1].path.file_name().map(Segment::as_bytes),
                path.file_name().map(Segment::as_bytes),
                "{} did not survive",
                name.id
            );
            assert_eq!(back.slots[&1].path, path, "{}", name.id);
        }
    }

    /// A v1 body does not carry `active`, and that means exactly "no
    /// profile". Reading it has to keep working: taking the session away
    /// from whoever updates the binary is exactly what ADR 0059 promises
    /// does not happen.
    #[test]
    fn a_v1_body_reads_as_no_profile() {
        let v1 = serde_json::json!({ "version": 1, "layouts": {}, "slots": {} });
        let b = SessionBody::from_value(1, &v1).expect("a v1 still reads");
        assert_eq!(b.active, "", "no profile, which is the truth");
    }

    #[test]
    fn the_active_profile_survives_the_trip() {
        let mut b = SessionBody {
            active: "work".to_owned(),
            ..SessionBody::default()
        };
        b.layouts
            .insert("work".into(), Node::slot(SlotId(1), KindId::browser()));
        let back = SessionBody::from_value(SCHEMA_VERSION, &b.to_value()).expect("round trip");
        assert_eq!(back.active, "work");
    }

    /// Spec 2026-09-15 D5/D6: the jump point and the populars make the trip,
    /// and a body written before they existed still reads without them.
    #[test]
    fn the_jump_point_and_the_populars_make_the_trip() {
        let mut b = SessionBody::default();
        b.slots.insert(
            1,
            SlotState {
                jump: Some(vp("file:///marcado")),
                ..slot("file:///casa")
            },
        );
        b.popular.push(crate::history::PopularEntry {
            path: vp("file:///frecuente"),
            visits: 3,
            last: 9,
        });
        let back = SessionBody::from_value(SCHEMA_VERSION, &b.to_value()).expect("round trip");
        assert_eq!(back.slots[&1].jump, Some(vp("file:///marcado")));
        assert_eq!(back.popular, b.popular);

        let old = serde_json::json!({
            "layouts": {},
            "slots": { "1": { "path": "file:///casa" } },
        });
        let b = SessionBody::from_value(SCHEMA_VERSION, &old).expect("an old body loads");
        assert_eq!(b.slots[&1].jump, None);
        assert!(b.popular.is_empty());
    }

    #[test]
    fn pruning_leaves_the_populars_at_their_cap_by_importance() {
        let mut b = SessionBody {
            popular: (0..crate::history::POPULAR_CAP + 5)
                .map(|i| crate::history::PopularEntry {
                    path: vp(&format!("file:///d{i}")),
                    // The first five are the LEAST visited: the ones that go.
                    visits: if i < 5 { 1 } else { 2 },
                    last: u64::try_from(i).expect("fits"),
                })
                .collect(),
            ..SessionBody::default()
        };
        b.prune(0);
        assert_eq!(b.popular.len(), crate::history::POPULAR_CAP);
        assert!(b.popular.iter().all(|e| e.visits == 2));
    }

    /// An unreadable popular entry is skipped: it does not take down the
    /// whole session with it (encoding-auditor, phase 1).
    #[test]
    fn an_unreadable_popular_is_skipped_and_the_body_loads() {
        let v = serde_json::json!({
            "layouts": {},
            "slots": { "1": { "path": "file:///casa" } },
            "popular": [
                { "path": "not a path", "visits": 9 },
                { "path": "file:///bien", "visits": 2, "last": 1 },
            ],
        });
        let b = SessionBody::from_value(SCHEMA_VERSION, &v).expect("the body loads");
        assert_eq!(
            b.slots[&1].path,
            vp("file:///casa"),
            "the slots are still there"
        );
        assert_eq!(b.popular.len(), 1);
        assert_eq!(b.popular[0].path, vp("file:///bien"));
    }

    /// And a body from the FUTURE is still refused whole: bumping to 2
    /// cannot open the door to a 3.
    #[test]
    fn a_v3_body_is_still_refused() {
        let v3 = serde_json::json!({ "layouts": {}, "slots": {}, "active": "x" });
        assert!(matches!(
            SessionBody::from_value(SCHEMA_VERSION + 1, &v3),
            Err(SessionError::FromTheFuture { .. })
        ));
    }

    /// A kind this binary does not declare comes back intact, `params`
    /// included: the session stores the tree, it does not interpret it (ADR
    /// 0058).
    #[test]
    fn an_unknown_kind_comes_back_whole() {
        let mut params = crate::layout::Params::new();
        params.set("grados", serde_json::json!(3));
        params.set("lo_que_sea", serde_json::json!({ "x": [1, 2] }));
        // With a listing next to it: a tree with none does not read (#242),
        // and what this test pins is that the FOREIGN kind comes back
        // intact.
        let tree = Node::split(
            crate::layout::Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::Slot {
                    id: SlotId(4),
                    kind: KindId::new("kind-de-otro-binario"),
                    params,
                    bindings: crate::layout::Bindings::default(),
                },
            ],
        );
        let mut b = SessionBody::default();
        b.layouts.insert("default".into(), tree.clone());
        let back = SessionBody::from_value(SCHEMA_VERSION, &b.to_value()).expect("parses");
        assert_eq!(back.layouts["default"], tree);
    }

    /// History is trimmed on WRITE, and from the old end: what gets dropped
    /// is the farthest away, not what was just walked.
    #[test]
    fn history_is_trimmed_from_the_old_end() {
        let mut b = SessionBody::default();
        let mut s = slot("file:///casa");
        s.back = (0..HISTORY_CAP + 10)
            .map(|i| vp(&format!("file:///d{i}")))
            .collect();
        b.slots.insert(1, s);
        b.layouts
            .insert("default".into(), Node::slot(SlotId(1), KindId::browser()));
        b.prune(0);
        let back = &b.slots[&1].back;
        assert_eq!(back.len(), HISTORY_CAP);
        assert_eq!(
            back.last().expect("last one"),
            &vp(&format!("file:///d{}", HISTORY_CAP + 9))
        );
    }

    /// And an ORPHAN's is trimmed more (#304): nobody can press "back"
    /// inside a slot no layout mentions without reopening it first. Dropped
    /// from the same end, the old one.
    #[test]
    fn an_orphans_history_is_trimmed_more() {
        let mut b = SessionBody::default();
        let mut s = slot("file:///casa");
        s.back = (0..HISTORY_CAP)
            .map(|i| vp(&format!("file:///d{i}")))
            .collect();
        s.forward = s.back.clone();
        b.slots.insert(1, s);
        b.layouts
            .insert("default".into(), Node::slot(SlotId(2), KindId::browser()));
        b.prune(0);
        let s = &b.slots[&1];
        assert_eq!(s.back.len(), ORPHAN_HISTORY_CAP);
        assert_eq!(s.forward.len(), ORPHAN_HISTORY_CAP);
        assert_eq!(
            s.back.last().expect("last one"),
            &vp(&format!("file:///d{}", HISTORY_CAP - 1)),
            "the recent one stays"
        );
    }

    /// A real clock, not zero: with `now_ms == 0` the sweep by age sweeps
    /// NOTHING, so a test that prunes at zero does not prove the orphan
    /// survives — it proves the subtraction never happened.
    const AHORA: u64 = 1_750_000_000_000;

    /// A layout that does not mention a slot does NOT erase its state:
    /// changing layouts does not drop your history.
    #[test]
    fn orphan_state_survives_a_layout_change() {
        let mut b = SessionBody::default();
        let mut orphan = slot("file:///lejos");
        orphan.touched_ms = AHORA;
        b.slots.insert(7, orphan);
        b.layouts
            .insert("default".into(), Node::slot(SlotId(1), KindId::browser()));
        b.prune(AHORA);
        assert!(b.slots.contains_key(&7), "the orphan stays");
    }

    /// And the one NOBODY has ever touched — `touched_ms` at zero against a
    /// real clock — goes: without sealing the mark on capture, this takes
    /// down every orphan on the first dump.
    #[test]
    fn an_unsealed_orphan_is_swept_against_a_real_clock() {
        let mut b = SessionBody::default();
        b.slots.insert(7, slot("file:///lejos"));
        b.layouts
            .insert("default".into(), Node::slot(SlotId(1), KindId::browser()));
        b.prune(AHORA);
        assert!(
            !b.slots.contains_key(&7),
            "with no seal there is no valid age"
        );
    }

    /// Orphans have a cap, and the one touched least recently falls.
    #[test]
    fn orphans_have_a_cap_and_the_oldest_falls() {
        let mut b = SessionBody::default();
        let cap = u32::try_from(ORPHAN_CAP).expect("fits");
        for i in 0..cap + 5 {
            let mut s = slot("file:///casa");
            s.touched_ms = u64::from(i);
            b.slots.insert(i, s);
        }
        b.prune(1_000);
        assert_eq!(b.slots.len(), ORPHAN_CAP);
        assert!(!b.slots.contains_key(&0), "the oldest one left");
        assert!(b.slots.contains_key(&(cap + 4)));
    }

    /// And an age: thirty days untouched and the slot goes, even if it fits.
    #[test]
    fn a_slot_thirty_days_old_is_swept() {
        let mut b = SessionBody::default();
        let mut old = slot("file:///casa");
        old.touched_ms = 0;
        let mut new = slot("file:///casa");
        new.touched_ms = MAX_AGE_MS;
        b.slots.insert(1, old);
        b.slots.insert(2, new);
        b.prune(MAX_AGE_MS + 1);
        assert!(!b.slots.contains_key(&1));
        assert!(b.slots.contains_key(&2));
    }

    /// A slot the LIVE layout mentions is swept by neither age nor cap: what
    /// is on screen is not recycled.
    #[test]
    fn a_visible_slot_is_never_swept() {
        let mut b = SessionBody::default();
        let mut s = slot("file:///casa");
        s.touched_ms = 0;
        b.slots.insert(1, s);
        b.layouts
            .insert("default".into(), Node::slot(SlotId(1), KindId::browser()));
        b.prune(AHORA);
        assert!(b.slots.contains_key(&1));
    }

    /// A sort COLUMN this binary does not know does not take down the whole
    /// screen: it falls back to the default order and everything else comes
    /// back.
    ///
    /// The fixture used to be `extension` until #138 built it, which is
    /// exactly the case this tolerance exists to cover: yesterday's
    /// hypothetical column is today's real one, and an old binary still has
    /// to open the session a new one wrote.
    #[test]
    fn an_unknown_sort_column_does_not_take_down_the_body() {
        let v = serde_json::json!({
            "version": SCHEMA_VERSION,
            "layouts": {},
            "slots": { "1": {
                "path": "file:///casa",
                "back": ["file:///antes"],
                "sort": { "column": "creacion", "dir": "asc", "dirs_first": true },
            }},
        });
        let b = SessionBody::from_value(SCHEMA_VERSION, &v).expect("parses");
        assert_eq!(b.slots[&1].sort, SortSpec::default());
        assert_eq!(b.slots[&1].path, vp("file:///casa"));
        assert_eq!(
            b.slots[&1].back,
            vec![vp("file:///antes")],
            "and the history"
        );
    }

    /// A body of a version this binary does not know is refused: better to
    /// start from the config than to interpret fields that are not yours.
    #[test]
    fn a_schema_from_the_future_is_refused() {
        let v = serde_json::json!({ "version": SCHEMA_VERSION + 1, "layouts": {}, "slots": {} });
        assert!(matches!(
            SessionBody::from_value(SCHEMA_VERSION, &v),
            Err(SessionError::FromTheFuture { .. })
        ));
    }

    /// A column this binary cannot read does not take down the whole screen
    /// with it: it is dropped and the rest comes back.
    #[test]
    fn an_unknown_column_is_dropped_without_taking_down_the_body() {
        let v = serde_json::json!({
            "version": SCHEMA_VERSION,
            "layouts": {},
            "slots": { "1": {
                "path": "file:///casa",
                "columns": ["name", "columna-de-otro-binario"],
            }},
        });
        let b = SessionBody::from_value(SCHEMA_VERSION, &v).expect("parses");
        assert_eq!(b.slots[&1].columns, vec!["name".parse().expect("name")]);
        assert_eq!(b.slots[&1].path, vp("file:///casa"));
    }
}
