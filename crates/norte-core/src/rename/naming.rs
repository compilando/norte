//! The two derived strings of a plan: the name of a temporary, and the hash of
//! the plan's conclusions. Both are pure functions of things the caller already
//! has, and both are DETERMINISTIC — re-planning the same intent against the
//! same directory answers the same bytes, which is what makes `plan_hash`
//! usable as a "the human read exactly this" token.

use std::collections::HashSet;

use sha2::{Digest, Sha256};

use super::plan::{Collision, NameCaps, Step, name_key};

/// The prefix every planner-owned temporary carries. It is deliberately
/// recognisable: after a crash a human must be able to tell machinery from
/// their own files (design §8, no automatic sweep).
pub(crate) const TEMP_PREFIX: &[u8] = b".norte-rename-";

/// A field fed to a digest with its length in front, so that two adjacent
/// fields can never be read as one (`"ab"+"c"` vs `"a"+"bc"`). Same discipline
/// as the journal's hash chain — see `crate::journal::feed`.
fn feed(h: &mut Sha256, bytes: &[u8]) {
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

/// Lowercase hex, the form `plan_hash` travels in on the wire.
pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(HEX[usize::from(b >> 4)]));
        s.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    s
}

/// The eight hex digits shared by every temporary of one plan: the first four
/// bytes of a digest of the INTENT — the pairs, sorted, plus the case regime.
///
/// Of the intent and NOT of the steps, and that is the whole point: the steps
/// contain the temporary, so hashing them would be circular and a re-plan could
/// not reproduce the name. Of the SORTED pairs so that the same intent listed
/// in another order still parks its file under the same name.
pub(crate) fn intent_tag(pairs: &[(Vec<u8>, Vec<u8>)], caps: NameCaps) -> String {
    let mut sorted: Vec<&(Vec<u8>, Vec<u8>)> = pairs.iter().collect();
    sorted.sort();
    let mut h = Sha256::new();
    h.update(b"norte-rename-temp-v1");
    feed(&mut h, &[u8::from(caps.case_sensitive)]);
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
            // `next` counts cycles plus squatters; 4096 pairs cannot overflow
            // it, and `saturating_add` refuses to wrap if anything ever tried.
            self.next = self.next.saturating_add(1);
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
/// Deliberately NOT the listing. A file that appears next door changes no
/// verdict, so the plan the human read is still the plan; a file that lands on
/// a destination turns a step into an `External` verdict, so it is not. That
/// asymmetry is the whole reason the hash is over conclusions.
pub(crate) fn plan_hash(steps: &[Step], collisions: &[Collision], caps: NameCaps) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"norte-rename-plan-v1");
    feed(&mut h, &[u8::from(caps.case_sensitive)]);
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
