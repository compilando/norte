//! The two derived strings of a plan: the name of a temporary, and the hash of
//! the plan's conclusions. Both are pure functions of things the caller already
//! has, and both are DETERMINISTIC — re-planning the same intent against the
//! same directory answers the same bytes, which is what makes `plan_hash`
//! usable as a "the human read exactly this" token.

use std::collections::HashSet;

use sha2::{Digest, Sha256};

use norte_encoding::FoldMode;

use super::plan::{Collision, NameCaps, Step, name_key};
use crate::hashing::{feed, hex_lower};

/// The prefix every planner-owned temporary carries. It is deliberately
/// recognisable: after a crash a human must be able to tell machinery from
/// their own files (design §8, no automatic sweep).
pub(crate) const TEMP_PREFIX: &[u8] = b".norte-rename-";

/// The eight hex digits shared by every temporary of one plan: the first four
/// bytes of a digest of the INTENT — the pairs, sorted, plus the case regime.
///
/// Of the intent and NOT of the steps, and that is the whole point: the steps
/// contain the temporary, so hashing them would be circular and a re-plan could
/// not reproduce the name. Of the SORTED pairs so that reordering the batch
/// does not move every temporary to a new tag.
///
/// The tag is order-independent; the FULL temporary name is not. `-<n>` is
/// handed out in the order [`super::plan::plan_batch`] meets the cycles, so a
/// batch with TWO cycles listed the other way round parks the same two files
/// under `-0` and `-1` swapped. One cycle — the common case, and the one
/// `the_temporary_does_not_depend_on_the_order_of_the_pairs` pins — always
/// lands on the same name.
/// The byte with which the directory's folding enters both digests.
///
/// The `None` and `Simple` values are the ones `u8::from(case_sensitive)`
/// used to have —1 and 0— **on purpose**: a plan computed before full
/// folding existed has to keep giving the same `plan_hash`, or the executor
/// would reject a plan the human just approved with an earlier binary.
/// `Full` gets a brand-new value, which is correct: a plan planned over a
/// `+F` is NOT the same plan.
fn fold_byte(caps: NameCaps) -> u8 {
    match caps.fold {
        FoldMode::Simple => 0,
        FoldMode::None => 1,
        FoldMode::Full => 2,
    }
}

pub(crate) fn intent_tag(pairs: &[(Vec<u8>, Vec<u8>)], caps: NameCaps) -> String {
    let mut sorted: Vec<&(Vec<u8>, Vec<u8>)> = pairs.iter().collect();
    sorted.sort();
    let mut h = Sha256::new();
    h.update(b"norte-rename-temp-v1");
    feed(&mut h, &[fold_byte(caps)]);
    for (from, to) in sorted {
        feed(&mut h, from);
        feed(&mut h, to);
    }
    hex_lower(&h.finalize()[..4])
}

/// Hands out `.norte-rename-<tag>-<n>` names that are free in the directory.
///
/// `n` climbs past anything already taken — a real file that looks like a
/// temporary, or a temporary this same plan already issued. The name never
/// embeds a base name, so it is ~25 bytes whatever the real names are and the
/// 255-byte component limit is simply out of reach.
pub(crate) struct TempNames {
    tag: String,
    next: u32,
    caps: NameCaps,
}

impl TempNames {
    /// A fresh dispenser for one plan.
    pub(crate) fn new(tag: String, caps: NameCaps) -> Self {
        Self { tag, next: 0, caps }
    }

    /// The next free temporary. `taken` holds comparison keys (see
    /// [`name_key`]) and gains the name that is handed out, so two cycles of
    /// one plan never receive the same detour.
    pub(crate) fn next(&mut self, taken: &mut HashSet<Vec<u8>>) -> Vec<u8> {
        loop {
            let mut name = Vec::with_capacity(TEMP_PREFIX.len() + self.tag.len() + 4);
            name.extend_from_slice(TEMP_PREFIX);
            name.extend_from_slice(self.tag.as_bytes());
            name.push(b'-');
            name.extend_from_slice(self.next.to_string().as_bytes());
            // INVARIANT: the loop only turns again when `taken.insert` refuses,
            // which happens at most once per entry already in `taken`, so it
            // runs at most `taken.len() + 1` times and `next` cannot exceed the
            // size of an in-memory `HashSet`. `checked_add` and not
            // `saturating_add`: saturating does not "refuse to wrap", it pins
            // `next` at `u32::MAX` and rebuilds one name that is already taken
            // for ever, and a hang in a pure planner is the worst of the three
            // possible answers.
            let Some(bumped) = self.next.checked_add(1) else {
                unreachable!("`taken` cannot hold 2^32 names in one directory")
            };
            self.next = bumped;
            let key = name_key(&name, self.caps).into_owned();
            if taken.insert(key) {
                return name;
            }
        }
    }
}

/// sha256 over the plan's CONCLUSIONS: the ordered steps, the collision
/// verdicts, and the case regime that produced them.
///
/// Deliberately NOT the listing. A file that appears next door usually changes
/// no verdict, so the plan the human read is still the plan; a file that lands
/// on a destination turns a step into an `External` verdict, so it is not. That
/// asymmetry is the whole reason the hash is over conclusions. "Usually",
/// because a new file can also squat the temporary a cycle was going to use,
/// which pushes the detour to `-1` and so does change a step — and therefore
/// the hash. That is correct: the steps really are different ones.
///
/// [`Step::pair_index`] is deliberately NOT fed in. It is a pure function of
/// the `pairs` the executor re-submits, so it cannot make two plans differ that
/// the rest of the digest calls equal, and it is not on the wire — the hash
/// stays a digest of exactly what the human was shown.
pub(crate) fn plan_hash(steps: &[Step], collisions: &[Collision], caps: NameCaps) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"norte-rename-plan-v1");
    feed(&mut h, &[fold_byte(caps)]);
    feed(&mut h, &(steps.len() as u64).to_le_bytes());
    for s in steps {
        feed(&mut h, &s.from);
        feed(&mut h, &s.to);
        feed(&mut h, &[u8::from(s.temp)]);
    }
    feed(&mut h, &(collisions.len() as u64).to_le_bytes());
    for c in collisions {
        feed(&mut h, &c.name);
        feed(&mut h, &[c.kind.tag()]);
        feed(&mut h, &c.pair_index.to_le_bytes());
    }
    h.finalize().into()
}
