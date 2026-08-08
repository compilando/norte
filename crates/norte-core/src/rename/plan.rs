//! The pure part of a batch rename: names in, an ordered plan out.
//!
//! No I/O, no `async`, no provider and no protocol types — the input is raw
//! name bytes plus the directory's listing plus what that directory says about
//! names, and the output is what to do and in what order. Everything that can
//! be got wrong about a batch of renames (equality of names, collisions,
//! ordering, cycles) is decided here, where it costs nothing to test.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use unicode_normalization::{UnicodeNormalization, is_nfc};

use super::naming::{TempNames, intent_tag, plan_hash};
use crate::hashing::hex_lower;

/// What the DESTINATION DIRECTORY says about names.
///
/// It comes from `fs.capabilities` of that directory — `norte-vfs-local` probes
/// it per directory (`pathconf` `_PC_CASE_SENSITIVE` on macOS, a real probe
/// elsewhere) — and never from `cfg!(target_os)`: one machine routinely has
/// both regimes mounted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameCaps {
    /// `true` when `Foo` and `foo` are two names in this directory.
    pub case_sensitive: bool,
}

/// Why a plan cannot run. Mirrors `norte_proto::methods::RenameCollisionKind`;
/// the vocabulary is CLOSED.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollisionKind {
    /// An earlier pair of this same batch makes this one impossible: it took
    /// the destination, or it took the source.
    ///
    /// Both are measured with the directory's COLLISION equality ([`name_key`])
    /// and not byte for byte, so two pairs naming two genuinely different files
    /// that this directory cannot tell apart land here as well. On a twin
    /// directory that means the two twins cannot be renamed in one batch —
    /// erring towards a verdict, as everywhere else folded equality is used.
    Internal,
    /// The destination already exists in the directory and no pair of this
    /// batch is going to move it out of the way.
    ///
    /// A pair whose SOURCE is that name is not enough to clear it: a null pair
    /// emits no step, and a source that is merely a twin of the blocker leaves
    /// the blocker exactly where it was. Vacating is a property of bytes.
    External,
    /// The source is not in the listing — the plan was built against a stale
    /// directory.
    AbsentSource,
    /// The source matches no listing entry exactly and folds onto TWO OR MORE
    /// of them: a `café` in NFC and a `café` in NFD sharing an ext4 directory,
    /// or `Foo` and `foo` in a listing that contradicts its own `NameCaps`.
    ///
    /// The source does not go missing here, it doubles. Guessing which twin was
    /// meant is a coin flip that renames the wrong file, so the planner refuses
    /// and says why. What clears THIS verdict is asking for the name byte for
    /// byte as the listing gives it: an exact match beats every fold and the
    /// source is identified.
    ///
    /// It is not a master key to the twin directory. Renaming BOTH twins in one
    /// batch still cannot be, because collisions are measured folded and the
    /// second twin collects an [`Self::Internal`]; send them in two batches.
    AmbiguousSource,
}

impl CollisionKind {
    /// A stable byte for the hash. Not a wire value: the wire form is the
    /// snake-case string of `RenameCollisionKind`.
    pub(crate) fn tag(self) -> u8 {
        match self {
            Self::Internal => 1,
            Self::External => 2,
            Self::AbsentSource => 3,
            Self::AmbiguousSource => 4,
        }
    }
}

/// A rejected pair and the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    /// The offending name. What it names depends on `kind`:
    ///
    /// - `Internal` — the destination the rejected pair will not get;
    /// - `External` — the name of the file that is IN THE WAY, as the directory
    ///   spells it, which is not necessarily how the request spelled it (an NFD
    ///   twin blocks an NFC destination, and the user needs to see the twin);
    /// - `AbsentSource` / `AmbiguousSource` — the source, as the CALLER wrote
    ///   it, since that is the text they have to correct.
    pub name: Vec<u8>,
    /// The verdict.
    pub kind: CollisionKind,
    /// Index, into the caller's `pairs`, of the pair this verdict rejects. For
    /// `Internal` it is the LATER pair — the one that lost, not the one that
    /// kept the destination.
    pub pair_index: u32,
}

/// One rename, in execution order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// The name before this step, ALWAYS the on-disk spelling: the bytes the
    /// directory listing gave, never the caller's spelling of them. A request
    /// typed NFC against an NFD name on disk is renamed by the NFD bytes,
    /// because on a filesystem that does not normalise the other one is not a
    /// file. An executor hands this straight to `Provider::rename`.
    pub from: Vec<u8>,
    /// The name after this step.
    pub to: Vec<u8>,
    /// `true` when EITHER side is a planner-owned temporary. It is a property
    /// of the STEP, not of the destination: the step that parks a file under a
    /// machine name and the step that brings it back out are both machinery,
    /// and a frontend must render neither as something the user asked for.
    pub temp: bool,
    /// Which requested pair this step descends from — an index into the
    /// caller's `pairs`. Every step has exactly one, temporaries included:
    /// BOTH halves of a broken cycle carry the index of the pair whose file
    /// took the detour.
    ///
    /// It is what lets an executor say *which row* failed when a `rename` dies
    /// mid-batch, report progress per requested rename rather than per step,
    /// and write a journal entry a human can read back (rule 4). Core-only:
    /// the wire `RenameStep` deliberately carries no index, because a frontend
    /// renders the user's own pairs plus the verdicts and never a step-to-row
    /// map.
    pub pair_index: u32,
}

/// What the planner decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenamePlan {
    /// The renames to run, in order, temporaries included. EMPTY whenever
    /// `collisions` is not: a dead plan is never half-ordered, so a caller who
    /// ignores [`executable`](Self::executable) has nothing to execute.
    pub steps: Vec<Step>,
    /// Everything that stops the plan, ordered by pair index. At most one per
    /// pair.
    pub collisions: Vec<Collision>,
    /// sha256 of the plan's CONCLUSIONS — the steps, the verdicts and the case
    /// regime. The token the caller approves and hands back to the executor,
    /// which re-plans against the directory as it is NOW and compares.
    pub hash: [u8; 32],
}

impl RenamePlan {
    /// Can this plan run as it is? The normative field; `steps` being empty is
    /// also the answer to "nothing to do".
    #[must_use]
    pub fn executable(&self) -> bool {
        self.collisions.is_empty()
    }

    /// [`hash`](Self::hash) as lowercase hex, the form it travels in.
    #[must_use]
    pub fn hash_hex(&self) -> String {
        hex_lower(&self.hash)
    }
}

/// The key two names are compared by when deciding whether they COLLIDE.
///
/// NFC when the name is valid UTF-8 — macOS hands out NFD and the same name
/// typed elsewhere is NFC, and on APFS they are ONE file — plus a lowercase
/// fold when the directory does not distinguish case. A name that is not UTF-8
/// is its own bytes: never normalised, never folded (hard rule 1).
///
/// **This is the collision equality, not the "did anything change" equality**,
/// and the planner deliberately uses two. Folded here, it can OVER-report on a
/// filesystem that does not normalise: a plan aiming at `café` is refused when
/// an NFD twin sits in the directory, even though ext4 would have held both.
/// That direction is safe — the user gets a verdict and nothing is clobbered.
/// Whether a step is a no-op is decided on RAW BYTES instead
/// ([`plan_batch`]), because there the failure mode is the opposite one:
/// silently not doing the work that was asked for. Do not unify them.
///
/// This only decides equality. What gets renamed is always the original bytes.
///
/// ```
/// use norte_core::rename::plan::{NameCaps, name_key};
/// let insensitive = NameCaps { case_sensitive: false };
/// assert_eq!(name_key(b"Foo", insensitive).as_ref(), b"foo");
/// // Not UTF-8: its own bytes, whatever the directory says about case.
/// assert_eq!(name_key(b"A\xff", insensitive).as_ref(), b"A\xff");
/// ```
#[must_use]
pub fn name_key(name: &[u8], caps: NameCaps) -> Cow<'_, [u8]> {
    let Ok(s) = std::str::from_utf8(name) else {
        return Cow::Borrowed(name);
    };
    let nfc: Cow<'_, str> = if is_nfc(s) {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(s.nfc().collect::<String>())
    };
    // An all-ASCII name with no uppercase byte already folds to itself; anything
    // else has to be asked, because `to_lowercase` moves more than the letters
    // `char::is_uppercase` admits to (titlecase digraphs, for one).
    let needs_fold =
        !caps.case_sensitive && (!nfc.is_ascii() || nfc.bytes().any(|b| b.is_ascii_uppercase()));
    if needs_fold {
        return Cow::Owned(nfc.to_lowercase().into_bytes());
    }
    match nfc {
        Cow::Borrowed(_) => Cow::Borrowed(name),
        Cow::Owned(o) => Cow::Owned(o.into_bytes()),
    }
}

/// A pair that survived classification and is real work.
struct Work {
    /// Index into the caller's `pairs`.
    index: usize,
    /// The bytes the file carries in the DIRECTORY, which are what a `rename`
    /// has to be given — not the caller's spelling of them. The two differ when
    /// the disk holds NFD and the caller typed NFC.
    from: Vec<u8>,
    to: Vec<u8>,
    from_key: Vec<u8>,
    to_key: Vec<u8>,
}

/// `pair_index` is a `u32` on the wire and `FS_RENAME_BATCH_MAX_PAIRS` is 4096,
/// so the index always fits. The saturating fallback exists so that no cast can
/// silently truncate if that bound ever moves.
fn pair_index(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

/// The directory, indexed BOTH ways: byte-exact and folded.
///
/// One index is not enough, and the missing one was a real bug. A folded index
/// alone collapses `café` NFC and `café` NFD into one slot, so a request naming
/// one of them resolves to whichever the listing happened to yield first — the
/// wrong file, silently, and precisely on the twin directory the NFC/NFD rules
/// exist for.
struct Listing<'a> {
    /// Every entry, byte for byte.
    exact: HashSet<&'a [u8]>,
    /// [`name_key`] → every entry that folds onto it, in listing order. More
    /// than one means the directory holds names it cannot tell apart.
    folded: HashMap<Vec<u8>, Vec<&'a [u8]>>,
}

/// What the directory has to say about a name the caller asked for.
enum Resolved<'a> {
    /// It is this file, and these are the bytes to rename.
    One(&'a [u8]),
    /// Nothing folds onto it.
    Absent,
    /// Two or more entries fold onto it and none of them is it, byte for byte.
    Ambiguous,
}

impl<'a> Listing<'a> {
    fn index(listing: &'a [Vec<u8>], caps: NameCaps) -> Self {
        let mut folded: HashMap<Vec<u8>, Vec<&'a [u8]>> = HashMap::with_capacity(listing.len());
        let mut exact: HashSet<&'a [u8]> = HashSet::with_capacity(listing.len());
        for entry in listing {
            exact.insert(entry.as_slice());
            let twins = folded
                .entry(name_key(entry, caps).into_owned())
                .or_default();
            // A directory cannot hold one byte string twice, but this is an
            // ASSEMBLED slice: a cursor-paginated `fs.list` (#27) can hand the
            // same entry back on two pages when the directory mutates between
            // them. Counting it twice would invent a twin and refuse a rename
            // with `AmbiguousSource` over a directory that has no twins.
            if !twins.contains(&entry.as_slice()) {
                twins.push(entry.as_slice());
            }
        }
        Self { exact, folded }
    }

    /// Which file the caller meant, in three tiers.
    ///
    /// An EXACT match wins outright: the caller quoted the listing, there is
    /// nothing to interpret, and this is also the escape hatch out of
    /// [`Resolved::Ambiguous`]. Otherwise a single folded candidate is the
    /// useful macOS case — typed NFC, stored NFD, one file, no doubt. Two or
    /// more and the planner refuses rather than flips a coin.
    fn resolve(&self, name: &[u8], key: &[u8]) -> Resolved<'a> {
        if let Some(exact) = self.exact.get(name) {
            return Resolved::One(exact);
        }
        match self.folded.get(key).map(Vec::as_slice) {
            None | Some([]) => Resolved::Absent,
            Some([only]) => Resolved::One(only),
            Some(_) => Resolved::Ambiguous,
        }
    }

    /// The file in the way of a destination, if there is one: an entry under
    /// the destination's key that this batch is NOT moving out.
    ///
    /// `vacated` holds on-disk BYTES and not keys, and that is the point on a
    /// twin directory. Two files can share one key, so moving one of them does
    /// not free the key: `cafe\u{301} → café` next to an existing NFC `café` is
    /// a rename onto an occupied name, not a cleanup, and a key-level "somebody
    /// vacates this" would have waved it through and clobbered a file.
    ///
    /// Of the survivors it prefers the one spelled exactly like the requested
    /// destination — an existing `café` blocking a requested `café` is reported
    /// as itself, not as its NFD twin — and otherwise names the first twin,
    /// which is still a file the user really has.
    fn blocker(&self, name: &[u8], key: &[u8], vacated: &HashSet<&'a [u8]>) -> Option<&'a [u8]> {
        let mut fallback = None;
        for &c in self.folded.get(key)? {
            if vacated.contains(c) {
                continue;
            }
            if c == name {
                return Some(c);
            }
            fallback = fallback.or(Some(c));
        }
        fallback
    }

    /// Every occupied key — the set a temporary has to stay out of.
    fn into_keys(self) -> HashSet<Vec<u8>> {
        self.folded.into_keys().collect()
    }
}

/// Plan a batch of renames inside ONE directory.
///
/// `pairs` are `from → to` in the caller's order (the order verdicts are
/// attributed in), `listing` is every name currently in the directory, and
/// `caps` is what that directory says about names.
///
/// The result either has collisions and NO steps, or steps and no collisions.
///
/// **Two different equalities, on purpose.** Whether a pair COLLIDES with
/// another name is asked through [`name_key`] — NFC, and case-folded where the
/// directory folds case — so that an NFD twin or a case twin is caught before
/// anything is clobbered. Whether a pair is a NO-OP is asked on raw bytes
/// against the name the directory actually holds, so that `cafe\u{301} → café`
/// is planned as the real work it is on a filesystem that does not normalise
/// (spec §17 character cleanup) and is a harmless repeat on one that does.
/// Folded equality erring towards a verdict is safe; byte equality is the only
/// safe answer to "was there anything to do".
///
/// **Verdicts are ROOT CAUSES, and they do not cascade.** One rejected pair can
/// make a second one unrunnable — `[(a,z), (b,a)]` against `[a,b,z]` reports
/// only that `z` is in the way, though `b→a` was equally doomed once `a→z` was
/// refused — and reporting the knock-on would bury the one line the user has to
/// act on under a list of consequences. Fix the cause, re-plan, see what is
/// left. `collisions` is therefore NOT "every pair that cannot run".
///
/// **The ORDER of `pairs` is part of the contract.** Verdicts are attributed in
/// it, `steps` follow it, the `-n` suffix of a temporary is assigned in it, and
/// all three feed [`RenamePlan::hash`]. A client that re-sorts a table between
/// previewing a plan and submitting it gets a different hash and therefore
/// `PlanStale` against a directory that never changed. Re-submit the pairs in
/// the order they were previewed.
///
/// # Preconditions
///
/// Each side of a pair must be a legal single directory entry: non-empty, no
/// `/`, no NUL, and neither `.` nor `..`. On the wire `Segment::new` enforces
/// this before the core is reached, but this module is `pub`, deliberately
/// proto-free, and both AI rename and the rules engine can call it directly —
/// so the obligation is the caller's. The planner treats names as opaque bytes
/// and would happily plan `to = "../../etc/passwd"`; a `debug_assert` catches
/// the whole list, and release builds do not check.
///
/// ```
/// use norte_core::rename::plan::{NameCaps, plan_batch};
/// let caps = NameCaps { case_sensitive: true };
/// // A swap: one temporary breaks the cycle, and it lands last.
/// let pairs = vec![(b"a".to_vec(), b"b".to_vec()), (b"b".to_vec(), b"a".to_vec())];
/// let plan = plan_batch(&pairs, &[b"a".to_vec(), b"b".to_vec()], caps);
/// assert!(plan.executable());
/// assert_eq!(plan.steps.len(), 3);
/// assert_eq!(plan.hash_hex().len(), 64);
/// ```
#[must_use]
pub fn plan_batch(pairs: &[(Vec<u8>, Vec<u8>)], listing: &[Vec<u8>], caps: NameCaps) -> RenamePlan {
    debug_assert!(
        pairs.iter().flat_map(|(f, t)| [f, t]).all(|n| {
            !n.is_empty()
                && n.as_slice() != b"."
                && n.as_slice() != b".."
                && !n.contains(&b'/')
                && !n.contains(&0)
        }),
        "a rename pair is two directory ENTRIES: non-empty, not `.`/`..`, \
         no `/`, no NUL (see Preconditions)",
    );
    let index = Listing::index(listing, caps);
    let (work, collisions) = classify(pairs, &index, caps);
    if !collisions.is_empty() {
        let hash = plan_hash(&[], &collisions, caps);
        return RenamePlan {
            steps: Vec::new(),
            collisions,
            hash,
        };
    }
    let steps = order(&work, index.into_keys(), intent_tag(pairs, caps), caps);
    let hash = plan_hash(&steps, &collisions, caps);
    RenamePlan {
        steps,
        collisions,
        hash,
    }
}

/// Pass one: every pair gets a verdict — work, dropped, or rejected.
fn classify<'a>(
    pairs: &[(Vec<u8>, Vec<u8>)],
    listing: &Listing<'a>,
    caps: NameCaps,
) -> (Vec<Work>, Vec<Collision>) {
    let mut work: Vec<Work> = Vec::with_capacity(pairs.len());
    let mut collisions = Vec::new();
    // Two different sets, and the difference is the point. `spoken_for` holds
    // KEYS: every source any pair has laid a claim on, null pairs included, and
    // it answers "is this batch contradicting itself". `vacated` holds on-disk
    // BYTES: only the files that actually MOVE, and it answers "will this name
    // be free". A null pair claims its source and vacates nothing, so
    // `[(a,a), (a,b)]` is a contradiction and `[(a,a), (b,a)]` still finds `a`
    // in the way.
    let mut spoken_for: HashSet<Vec<u8>> = HashSet::new();
    let mut vacated: HashSet<&'a [u8]> = HashSet::new();
    let mut claimed_dests: HashSet<Vec<u8>> = HashSet::new();
    let reject = |i: usize, name: &[u8], kind: CollisionKind| Collision {
        name: name.to_vec(),
        kind,
        pair_index: pair_index(i),
    };

    for (i, (from, to)) in pairs.iter().enumerate() {
        let from_key = name_key(from, caps).into_owned();
        let on_disk = match listing.resolve(from, &from_key) {
            Resolved::One(bytes) => bytes,
            Resolved::Absent => {
                collisions.push(reject(i, from, CollisionKind::AbsentSource));
                continue;
            }
            Resolved::Ambiguous => {
                collisions.push(reject(i, from, CollisionKind::AmbiguousSource));
                continue;
            }
        };
        // The claim comes BEFORE the no-op question, so a batch that says both
        // "`a` stays `a`" and "`a` becomes `b`" is caught whichever order the
        // two arrive in. `name` is the destination the loser will not get.
        if !spoken_for.insert(from_key.clone()) {
            collisions.push(reject(i, to, CollisionKind::Internal));
            continue;
        }
        // A null step is BYTE equality against the name that is really there,
        // and nothing else. Folded equality here would silently swallow the
        // work: on ext4 `café` and `cafe\u{301}` are two different files and
        // rewriting one into the other is exactly the character cleanup §17
        // asks for, while on APFS the same rename is a harmless no-op.
        // Refusing is wrong on one platform and pointless on the other.
        if on_disk == to.as_slice() {
            continue;
        }
        let to_key = name_key(to, caps).into_owned();
        if !claimed_dests.insert(to_key.clone()) {
            collisions.push(reject(i, to, CollisionKind::Internal));
            continue;
        }
        vacated.insert(on_disk);
        work.push(Work {
            index: i,
            from: on_disk.to_vec(),
            to: to.clone(),
            from_key,
            to_key,
        });
    }

    // Pass two: a destination that exists and that nothing in this batch is
    // going to move out of the way. It needs the whole batch to be classified
    // first — that is what saves a chain (`a→b, b→c`) and a case-only rename,
    // whose destination is its OWN source and is therefore in `vacated`. The
    // verdict names the BLOCKER as the directory spells it, which is not always
    // how the request spelled it.
    for w in &work {
        if let Some(blocker) = listing.blocker(&w.to, &w.to_key, &vacated) {
            collisions.push(reject(w.index, blocker, CollisionKind::External));
        }
    }
    collisions.sort_by_key(|c| c.pair_index);
    (work, collisions)
}

/// Pass three: order the work, breaking each cycle with one temporary.
///
/// Sources are unique and destinations are unique (classification rejected the
/// rest), so the dependency graph — "my destination is your source, you go
/// first" — has in- and out-degree at most one: disjoint chains and cycles.
/// Chains are walked from their free end backwards; what is left over is a pure
/// cycle, and a cycle is opened by parking one member under a temporary,
/// running the rest, and landing the temporary last.
fn order(work: &[Work], mut taken: HashSet<Vec<u8>>, tag: String, caps: NameCaps) -> Vec<Step> {
    let n = work.len();
    let sources: HashMap<&[u8], usize> = work
        .iter()
        .enumerate()
        .map(|(i, w)| (w.from_key.as_slice(), i))
        .collect();
    // `waiter[j] = i`: once j has moved, i's destination is free.
    let mut waiter: Vec<Option<usize>> = vec![None; n];
    let mut blocked = vec![false; n];
    for (i, w) in work.iter().enumerate() {
        if let Some(&j) = sources.get(w.to_key.as_slice()) {
            // `j == i` is a case-only rename (`Foo → foo`): it targets its own
            // source, which is not a dependency and not a cycle. One rename.
            if j != i {
                blocked[i] = true;
                waiter[j] = Some(i);
            }
        }
    }
    for w in work {
        taken.insert(w.to_key.clone());
    }
    let mut temps = TempNames::new(tag, caps);

    let mut done = vec![false; n];
    let mut steps: Vec<Step> = Vec::with_capacity(n);
    let run_from = |start: usize, steps: &mut Vec<Step>, done: &mut Vec<bool>| {
        let mut cur = Some(start);
        while let Some(c) = cur {
            if done[c] {
                break;
            }
            steps.push(Step {
                from: work[c].from.clone(),
                to: work[c].to.clone(),
                temp: false,
                pair_index: pair_index(work[c].index),
            });
            done[c] = true;
            cur = waiter[c];
        }
    };
    // Chains: start where the destination is already free. Only a blocked node
    // is ever anybody's waiter, so no unblocked node is reached twice.
    for (i, is_blocked) in blocked.iter().enumerate() {
        if !is_blocked {
            run_from(i, &mut steps, &mut done);
        }
    }
    // What is left cannot be reached from any free destination: pure cycles.
    for i in 0..n {
        if done[i] {
            continue;
        }
        let temp = temps.next(&mut taken);
        steps.push(Step {
            from: work[i].from.clone(),
            to: temp.clone(),
            temp: true,
            pair_index: pair_index(work[i].index),
        });
        done[i] = true;
        if let Some(next) = waiter[i] {
            run_from(next, &mut steps, &mut done);
        }
        steps.push(Step {
            from: temp,
            to: work[i].to.clone(),
            temp: true,
            pair_index: pair_index(work[i].index),
        });
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(b: &[u8]) -> Vec<u8> {
        b.to_vec()
    }

    fn pairs(v: &[(&[u8], &[u8])]) -> Vec<(Vec<u8>, Vec<u8>)> {
        v.iter().map(|(f, t)| (name(f), name(t))).collect()
    }

    const SENSITIVE: NameCaps = NameCaps {
        case_sensitive: true,
    };
    const INSENSITIVE: NameCaps = NameCaps {
        case_sensitive: false,
    };

    /// The ordinary case: three independent renames stay in one step each.
    #[test]
    fn independent_renames_need_no_temporaries() {
        let p = plan_batch(
            &pairs(&[(b"a", b"x"), (b"b", b"y")]),
            &[name(b"a"), name(b"b")],
            SENSITIVE,
        );
        assert!(p.executable(), "{:?}", p.collisions);
        assert_eq!(p.steps.len(), 2);
        assert!(p.steps.iter().all(|s| !s.temp));
    }

    /// A chain `a→b, b→c` is legal and ORDERED: `b→c` must run before `a→b`.
    /// This is the case the per-move loop could never do.
    #[test]
    fn a_chain_is_ordered_so_the_freed_name_comes_first() {
        let p = plan_batch(
            &pairs(&[(b"a", b"b"), (b"b", b"c")]),
            &[name(b"a"), name(b"b")],
            SENSITIVE,
        );
        assert!(p.executable(), "{:?}", p.collisions);
        assert_eq!(
            p.steps
                .iter()
                .map(|s| (s.from.clone(), s.to.clone()))
                .collect::<Vec<_>>(),
            vec![(name(b"b"), name(b"c")), (name(b"a"), name(b"b"))],
        );
        assert!(p.steps.iter().all(|s| !s.temp));
    }

    /// A permutation is a pure cycle: exactly ONE temporary breaks it, and the
    /// temporary lands last.
    #[test]
    fn a_two_cycle_is_broken_with_exactly_one_temporary() {
        let p = plan_batch(
            &pairs(&[(b"a", b"b"), (b"b", b"a")]),
            &[name(b"a"), name(b"b")],
            SENSITIVE,
        );
        assert!(p.executable(), "{:?}", p.collisions);
        assert_eq!(
            p.steps.len(),
            3,
            "two renames plus one detour: {:?}",
            p.steps
        );
        assert_eq!(p.steps.iter().filter(|s| s.temp).count(), 2);
        let last = p.steps.last().expect("a step");
        assert!(
            last.from.starts_with(b".norte-rename-"),
            "the temporary lands last",
        );
    }

    /// Two pairs targeting one name is an internal collision and stops the plan.
    #[test]
    fn two_pairs_targeting_one_name_is_an_internal_collision() {
        let p = plan_batch(
            &pairs(&[(b"a", b"z"), (b"b", b"z")]),
            &[name(b"a"), name(b"b")],
            SENSITIVE,
        );
        assert!(!p.executable());
        assert_eq!(
            p.collisions,
            vec![Collision {
                name: name(b"z"),
                kind: CollisionKind::Internal,
                pair_index: 1,
            }],
        );
        assert!(p.steps.is_empty(), "nothing is ordered for a dead plan");
    }

    /// A destination that already exists and is nobody's source is external.
    #[test]
    fn an_existing_bystander_destination_is_an_external_collision() {
        let p = plan_batch(
            &pairs(&[(b"a", b"z")]),
            &[name(b"a"), name(b"z")],
            SENSITIVE,
        );
        assert!(!p.executable());
        assert_eq!(p.collisions[0].kind, CollisionKind::External);
    }

    /// A source that is not in the listing means the plan was built against a
    /// stale directory.
    #[test]
    fn a_missing_source_is_an_absent_source_collision() {
        let p = plan_batch(&pairs(&[(b"gone", b"z")]), &[name(b"a")], SENSITIVE);
        assert!(!p.executable());
        assert_eq!(p.collisions[0].kind, CollisionKind::AbsentSource);
        assert_eq!(p.collisions[0].name, name(b"gone"));
    }

    /// `from == to` is dropped: it is not work, and it is not a collision.
    #[test]
    fn an_identity_pair_is_dropped_as_a_null_step() {
        let p = plan_batch(&pairs(&[(b"a", b"a")]), &[name(b"a")], SENSITIVE);
        assert!(p.executable());
        assert!(p.steps.is_empty());
    }

    /// Rewriting an NFD name into its NFC spelling is REAL work, not a null
    /// step: on a filesystem that does not normalise the two are two files, and
    /// this is the character cleanup §17 asks a batch rename for. On one that
    /// does normalise the same rename is a harmless no-op. Refusing it would be
    /// wrong on the first and pointless on the second.
    ///
    /// The pair still folds onto its own source, so it is not an external
    /// collision either — the same shape as a case-only rename below.
    #[test]
    fn an_nfd_to_nfc_rename_is_real_work() {
        let nfc = "café".as_bytes().to_vec();
        let nfd = "cafe\u{301}".as_bytes().to_vec();
        let p = plan_batch(
            &[(nfd.clone(), nfc.clone())],
            std::slice::from_ref(&nfd),
            SENSITIVE,
        );
        assert!(p.executable(), "{:?}", p.collisions);
        assert_eq!(p.steps.len(), 1, "{:?}", p.steps);
        assert_eq!(p.steps[0].from, nfd);
        assert_eq!(p.steps[0].to, nfc);
        assert!(!p.steps[0].temp, "one rename is enough at this layer");
    }

    /// The two names that fold onto their OWN source are one category, and the
    /// planner has to treat them alike: not external (the obstacle is the file
    /// being renamed), not a dependency (nothing has to move first), one plain
    /// step each.
    #[test]
    fn a_destination_that_folds_onto_its_own_source_is_one_plain_step() {
        let nfd = "cafe\u{301}".as_bytes().to_vec();
        let cases: [(Vec<u8>, Vec<u8>, NameCaps); 2] = [
            (nfd, "café".as_bytes().to_vec(), SENSITIVE),
            (name(b"Foo"), name(b"foo"), INSENSITIVE),
        ];
        for (from, to, caps) in cases {
            let p = plan_batch(
                &[(from.clone(), to.clone())],
                std::slice::from_ref(&from),
                caps,
            );
            assert!(p.executable(), "{:?}", p.collisions);
            assert_eq!(
                p.steps,
                vec![Step {
                    from,
                    to,
                    temp: false,
                    pair_index: 0,
                }],
            );
        }
    }

    /// The OTHER half of the asymmetry, and it is load-bearing: a destination
    /// that FOLDS onto a bystander is external even though the bytes differ.
    /// On ext4 the two spellings could have coexisted, so this over-reports —
    /// deliberately. The alternative is clobbering an NFD twin on macOS, where
    /// the two ARE one file. Byte-exact collision comparison would do exactly
    /// that; this test exists so nobody quietly introduces it.
    ///
    /// And the verdict names the BLOCKER's bytes, not the request's. Telling a
    /// user that `café` is in the way of `café` is telling them nothing; the
    /// two render identically and only the on-disk spelling is actionable.
    #[test]
    fn a_destination_that_folds_onto_a_bystander_is_external() {
        let nfc = "café".as_bytes().to_vec();
        let nfd = "cafe\u{301}".as_bytes().to_vec();
        let p = plan_batch(
            &[(name(b"x"), nfc.clone())],
            &[name(b"x"), nfd.clone()],
            SENSITIVE,
        );
        assert!(!p.executable(), "an NFD twin is in the way");
        assert_eq!(
            p.collisions,
            vec![Collision {
                name: nfd,
                kind: CollisionKind::External,
                pair_index: 0,
            }],
        );
        assert_ne!(
            nfc, p.collisions[0].name,
            "the blocker's bytes, not the ones that were asked for",
        );
    }

    /// The source is resolved to what the directory holds BEFORE the no-op
    /// question is asked. `Foo → foo` over a directory that actually holds
    /// `foo` is renaming a file onto its own name: nothing to do.
    #[test]
    fn a_case_only_rename_of_a_name_already_in_that_case_is_a_null_step() {
        let p = plan_batch(&pairs(&[(b"Foo", b"foo")]), &[name(b"foo")], INSENSITIVE);
        assert!(p.executable(), "{:?}", p.collisions);
        assert!(p.steps.is_empty(), "{:?}", p.steps);
    }

    /// `Foo → foo` on a case-INSENSITIVE directory is a real rename (the bytes
    /// change), not a null step — and the destination is not an external
    /// collision against its own source.
    #[test]
    fn a_case_only_rename_is_real_work_on_a_case_insensitive_directory() {
        let p = plan_batch(&pairs(&[(b"Foo", b"foo")]), &[name(b"Foo")], INSENSITIVE);
        assert!(p.executable(), "{:?}", p.collisions);
        assert_eq!(p.steps.len(), 1);
        assert!(!p.steps[0].temp, "one rename is enough at this layer");
    }

    /// On a case-insensitive directory, `a→B` collides with an existing `b`.
    /// On a case-sensitive one it does not. The DIRECTORY decides, not the OS
    /// this test runs on.
    #[test]
    fn case_insensitivity_is_decided_by_the_directory() {
        let insensitive = plan_batch(
            &pairs(&[(b"a", b"B")]),
            &[name(b"a"), name(b"b")],
            INSENSITIVE,
        );
        assert!(!insensitive.executable());
        assert_eq!(insensitive.collisions[0].kind, CollisionKind::External);

        let sensitive = plan_batch(
            &pairs(&[(b"a", b"B")]),
            &[name(b"a"), name(b"b")],
            SENSITIVE,
        );
        assert!(sensitive.executable(), "{:?}", sensitive.collisions);
    }

    /// Rule 1: a non-UTF-8 name is never normalised and never folded. It is
    /// compared byte to byte and it survives into the steps intact.
    #[test]
    fn non_utf8_names_are_compared_byte_to_byte() {
        let hostile = name(b"caf\xff\xfe.txt");
        let p = plan_batch(
            &[(hostile.clone(), name(b"ok.txt"))],
            std::slice::from_ref(&hostile),
            INSENSITIVE,
        );
        assert!(p.executable(), "{:?}", p.collisions);
        assert_eq!(p.steps[0].from, hostile);
    }

    /// The temporary is fixed-length and derived from the pairs, so it is the
    /// SAME across a plan and its re-plan, and it can never blow the 255-byte
    /// component limit no matter how long the real names are.
    #[test]
    fn the_temporary_name_is_short_and_stable_across_replans() {
        let long = name(&[b'x'; 250]);
        let other = name(&[b'y'; 250]);
        let listing = vec![long.clone(), other.clone()];
        let ps = vec![(long.clone(), other.clone()), (other, long)];
        let first = plan_batch(&ps, &listing, SENSITIVE);
        let again = plan_batch(&ps, &listing, SENSITIVE);
        let temp = first
            .steps
            .iter()
            .find(|s| s.temp)
            .expect("a temporary")
            .to
            .clone();
        assert!(temp.len() <= 40, "temp name is bounded: {}", temp.len());
        assert_eq!(first.hash, again.hash, "re-planning is deterministic");
        assert!(again.steps.iter().any(|s| s.to == temp));
    }

    /// A temporary steps aside for a squatter it merely FOLDS onto, not only
    /// for one that matches it byte for byte. On a case-insensitive directory
    /// `.NORTE-RENAME-…-0` and `.norte-rename-…-0` are one name, and issuing
    /// the second would be a rename onto an existing file.
    ///
    /// The squatter has to be a name the dispenser could really emit: a bare
    /// `.norte-rename-` prefix, with no tag and no `-n`, is unreachable, so a
    /// test using it passes with the `taken` check deleted.
    #[test]
    fn the_temporary_avoids_a_name_it_only_folds_onto() {
        let ps = pairs(&[(b"a", b"b"), (b"b", b"a")]);
        let listing = vec![name(b"a"), name(b"b")];
        let temp0 = plan_batch(&ps, &listing, INSENSITIVE)
            .steps
            .iter()
            .find(|s| s.temp)
            .expect("a temporary")
            .to
            .clone();
        let squatter = String::from_utf8(temp0.clone())
            .expect("the dispenser emits ASCII")
            .to_uppercase()
            .into_bytes();
        assert_ne!(squatter, temp0, "the squatter differs in case only");

        let mut occupied = listing.clone();
        occupied.push(squatter.clone());
        let p = plan_batch(&ps, &occupied, INSENSITIVE);
        assert!(p.executable(), "{:?}", p.collisions);
        for s in &p.steps {
            assert_ne!(s.to, squatter, "a temporary must not clobber a real name");
            assert_ne!(s.to, temp0, "nor the name that folds onto it");
        }
    }

    /// The hash covers CONCLUSIONS: an unrelated new file does not invalidate a
    /// plan, a file that creates a collision does.
    #[test]
    fn the_hash_ignores_irrelevant_drift_and_catches_relevant_drift() {
        let ps = pairs(&[(b"a", b"z")]);
        let base = plan_batch(&ps, &[name(b"a")], SENSITIVE);
        let unrelated = plan_batch(&ps, &[name(b"a"), name(b"unrelated")], SENSITIVE);
        assert_eq!(base.hash, unrelated.hash);
        let colliding = plan_batch(&ps, &[name(b"a"), name(b"z")], SENSITIVE);
        assert_ne!(base.hash, colliding.hash);
    }

    /// The hash separates the two case regimes: the same pairs and the same
    /// listing under a different directory answer a different plan.
    #[test]
    fn the_hash_covers_the_case_regime() {
        let ps = pairs(&[(b"a", b"B")]);
        let listing = [name(b"a")];
        assert_ne!(
            plan_batch(&ps, &listing, SENSITIVE).hash,
            plan_batch(&ps, &listing, INSENSITIVE).hash,
        );
    }

    // ---- beyond the floor ------------------------------------------------

    /// Three files rotating: still ONE temporary, and every plain step lands
    /// between the two that touch it.
    #[test]
    fn a_three_cycle_still_costs_a_single_temporary() {
        let p = plan_batch(
            &pairs(&[(b"a", b"b"), (b"b", b"c"), (b"c", b"a")]),
            &[name(b"a"), name(b"b"), name(b"c")],
            SENSITIVE,
        );
        assert!(p.executable(), "{:?}", p.collisions);
        assert_eq!(p.steps.len(), 4, "{:?}", p.steps);
        assert_eq!(p.steps.iter().filter(|s| s.temp).count(), 2);
        assert!(p.steps[0].temp && p.steps[3].temp, "{:?}", p.steps);
        assert_eq!(simulate(&p, &[name(b"a"), name(b"b"), name(b"c")]), {
            let mut want = vec![name(b"b"), name(b"c"), name(b"a")];
            want.sort();
            want
        });
    }

    /// Two swaps in one batch are two cycles, so two temporaries — and they
    /// must be DIFFERENT names, which is what `n` is for.
    #[test]
    fn two_disjoint_cycles_get_one_temporary_each() {
        let listing = vec![name(b"a"), name(b"b"), name(b"c"), name(b"d")];
        let p = plan_batch(
            &pairs(&[(b"a", b"b"), (b"b", b"a"), (b"c", b"d"), (b"d", b"c")]),
            &listing,
            SENSITIVE,
        );
        assert!(p.executable(), "{:?}", p.collisions);
        assert_eq!(p.steps.len(), 6, "{:?}", p.steps);
        let temps: HashSet<&Vec<u8>> = p.steps.iter().filter(|s| s.temp).map(|s| &s.to).collect();
        // Two landing steps carry a real destination, two carry a temporary.
        assert_eq!(temps.len(), 4, "two temps and two real names: {temps:?}");
        let issued: HashSet<&Vec<u8>> = p
            .steps
            .iter()
            .filter(|s| s.temp && s.to.starts_with(b".norte-rename-"))
            .map(|s| &s.to)
            .collect();
        assert_eq!(issued.len(), 2, "one temporary per cycle, and distinct");
        let mut want = vec![name(b"b"), name(b"a"), name(b"d"), name(b"c")];
        want.sort();
        assert_eq!(simulate(&p, &listing), want);
    }

    /// A cycle and an independent rename in one batch: the independent one does
    /// not pay for the cycle's detour.
    #[test]
    fn a_cycle_and_an_independent_rename_share_a_batch() {
        let listing = vec![name(b"a"), name(b"b"), name(b"c")];
        let p = plan_batch(
            &pairs(&[(b"a", b"b"), (b"b", b"a"), (b"c", b"x")]),
            &listing,
            SENSITIVE,
        );
        assert!(p.executable(), "{:?}", p.collisions);
        assert_eq!(p.steps.len(), 4, "{:?}", p.steps);
        assert_eq!(p.steps.iter().filter(|s| s.temp).count(), 2);
        let mut want = vec![name(b"b"), name(b"a"), name(b"x")];
        want.sort();
        assert_eq!(simulate(&p, &listing), want);
    }

    /// A pair aimed at a name that another pair merely CLAIMS to keep is an
    /// external collision: the null pair emits no step, so nothing frees the
    /// name and the file really is in the way.
    #[test]
    fn targeting_the_source_of_a_dropped_null_pair_is_external() {
        let p = plan_batch(
            &pairs(&[(b"a", b"a"), (b"b", b"a")]),
            &[name(b"a"), name(b"b")],
            SENSITIVE,
        );
        assert!(!p.executable());
        assert_eq!(
            p.collisions,
            vec![Collision {
                name: name(b"a"),
                kind: CollisionKind::External,
                pair_index: 1,
            }],
        );
    }

    /// One file cannot go to two places. The later pair is the one rejected,
    /// and the name it carries is the destination it will not get.
    #[test]
    fn one_source_claimed_twice_rejects_the_later_pair() {
        let p = plan_batch(
            &pairs(&[(b"a", b"x"), (b"a", b"y")]),
            &[name(b"a")],
            SENSITIVE,
        );
        assert!(!p.executable());
        assert_eq!(
            p.collisions,
            vec![Collision {
                name: name(b"y"),
                kind: CollisionKind::Internal,
                pair_index: 1,
            }],
        );
    }

    /// `Z` and `z` are two destinations on a case-sensitive directory and one
    /// on a case-insensitive one. The verdict follows the directory.
    #[test]
    fn two_destinations_that_differ_only_in_case_collide_only_where_case_folds() {
        let listing = [name(b"a"), name(b"b")];
        let ps = pairs(&[(b"a", b"Z"), (b"b", b"z")]);
        assert!(plan_batch(&ps, &listing, SENSITIVE).executable());
        let folded = plan_batch(&ps, &listing, INSENSITIVE);
        assert_eq!(
            folded.collisions,
            vec![Collision {
                name: name(b"z"),
                kind: CollisionKind::Internal,
                pair_index: 1,
            }],
        );
    }

    /// Every verdict is attributed, and they come back in pair order however
    /// they were found — absent sources and internal clashes in one pass,
    /// external ones only once the whole batch is known.
    #[test]
    fn collisions_come_back_in_pair_order() {
        let p = plan_batch(
            &pairs(&[(b"a", b"z"), (b"gone", b"q"), (b"b", b"y"), (b"c", b"z")]),
            &[name(b"a"), name(b"b"), name(b"c"), name(b"z")],
            SENSITIVE,
        );
        assert!(!p.executable());
        assert_eq!(
            p.collisions
                .iter()
                .map(|c| (c.pair_index, c.kind))
                .collect::<Vec<_>>(),
            vec![
                (0, CollisionKind::External),
                (1, CollisionKind::AbsentSource),
                (3, CollisionKind::Internal),
            ],
        );
        assert!(p.steps.is_empty(), "a dead plan carries no steps");
    }

    /// The step renames the bytes that are REALLY in the directory. A macOS
    /// listing hands out NFD; the request was typed NFC; the `rename` syscall
    /// gets the NFD name, because that is the file that exists.
    #[test]
    fn the_step_carries_the_bytes_the_directory_holds() {
        let nfc = "café".as_bytes().to_vec();
        let nfd = "cafe\u{301}".as_bytes().to_vec();
        let p = plan_batch(
            &[(nfc, name(b"coffee"))],
            std::slice::from_ref(&nfd),
            SENSITIVE,
        );
        assert!(p.executable(), "{:?}", p.collisions);
        assert_eq!(p.steps[0].from, nfd, "the name on disk, not the one typed");
        assert_eq!(p.steps[0].to, name(b"coffee"));
    }

    /// A source that is absent because the request spells it in another case on
    /// a case-SENSITIVE directory. The directory decides here too.
    #[test]
    fn a_source_in_the_wrong_case_is_absent_on_a_case_sensitive_directory() {
        let p = plan_batch(&pairs(&[(b"FOO", b"bar")]), &[name(b"foo")], SENSITIVE);
        assert_eq!(p.collisions[0].kind, CollisionKind::AbsentSource);
        assert!(plan_batch(&pairs(&[(b"FOO", b"bar")]), &[name(b"foo")], INSENSITIVE).executable());
    }

    /// Nothing asked for is nothing done — and still a well-formed plan.
    #[test]
    fn an_empty_batch_is_an_executable_plan_with_no_steps() {
        let p = plan_batch(&[], &[name(b"a")], SENSITIVE);
        assert!(p.executable());
        assert!(p.steps.is_empty());
        assert_eq!(p.hash_hex().len(), 64);
    }

    /// The temporary steps ASIDE for a real file that already owns the name it
    /// would have taken: `n` climbs.
    #[test]
    fn the_temporary_steps_aside_for_a_squatter_with_its_exact_name() {
        let ps = pairs(&[(b"a", b"b"), (b"b", b"a")]);
        let listing = vec![name(b"a"), name(b"b")];
        let first = plan_batch(&ps, &listing, SENSITIVE);
        let temp0 = first
            .steps
            .iter()
            .find(|s| s.temp)
            .expect("a temporary")
            .to
            .clone();
        assert!(
            temp0.ends_with(b"-0"),
            "{:?}",
            String::from_utf8_lossy(&temp0)
        );

        let mut squatted = listing.clone();
        squatted.push(temp0.clone());
        let second = plan_batch(&ps, &squatted, SENSITIVE);
        assert!(second.executable(), "{:?}", second.collisions);
        let temp1 = second
            .steps
            .iter()
            .find(|s| s.temp)
            .expect("a temporary")
            .to
            .clone();
        assert_ne!(temp1, temp0, "a temporary never clobbers a real file");
        assert!(temp1.ends_with(b"-1"));
        assert_ne!(first.hash, second.hash, "different steps, different plan");
    }

    /// A batch may legitimately ASK for a name that looks like planner
    /// machinery — an agent proposing `.norte-rename-…` is not a special case,
    /// it is just a name — and the plan still runs without the two treading on
    /// each other. The detour is reserved against the batch's destinations, not
    /// only against the directory.
    #[test]
    fn a_destination_that_looks_like_a_temporary_is_planned_anyway() {
        let ps = pairs(&[(b"a", b"b"), (b"b", b"a")]);
        let listing = vec![name(b"a"), name(b"b")];
        let temp0 = plan_batch(&ps, &listing, SENSITIVE)
            .steps
            .iter()
            .find(|s| s.temp)
            .expect("a temporary")
            .to
            .clone();
        // A third file aiming exactly at the name the detour would have used.
        let mut listing3 = listing.clone();
        listing3.push(name(b"c"));
        let mut ps3 = ps.clone();
        ps3.push((name(b"c"), temp0.clone()));
        let p = plan_batch(&ps3, &listing3, SENSITIVE);
        assert!(p.executable(), "{:?}", p.collisions);
        let detour = p
            .steps
            .iter()
            .find(|s| s.temp && s.to.starts_with(b".norte-rename-"))
            .expect("a temporary")
            .to
            .clone();
        assert_ne!(detour, temp0);
        assert_eq!(simulate(&p, &listing3), {
            let mut want = vec![name(b"b"), name(b"a"), temp0];
            want.sort();
            want
        });
    }

    /// `name_key` on its own: normalisation and folding are separate axes, and
    /// neither of them touches bytes that are not UTF-8.
    #[test]
    fn name_key_normalises_utf8_and_leaves_the_rest_alone() {
        let nfd = "cafe\u{301}".as_bytes();
        assert_eq!(name_key(nfd, SENSITIVE).as_ref(), "café".as_bytes());
        assert_eq!(name_key(b"Foo", SENSITIVE).as_ref(), b"Foo");
        assert_eq!(name_key(b"Foo", INSENSITIVE).as_ref(), b"foo");
        // Not UTF-8: no NFC, no fold, whatever the directory says.
        let hostile = b"CAF\xff";
        assert_eq!(name_key(hostile, INSENSITIVE).as_ref(), hostile);
        // A capital outside ASCII folds too — and to two code points, which is
        // why the ASCII shortcut cannot be the whole answer.
        assert_eq!(
            name_key("\u{130}".as_bytes(), INSENSITIVE).as_ref(),
            "i\u{307}".as_bytes(),
        );
    }

    /// The hex form is what the wire carries: 64 lowercase hex digits.
    #[test]
    fn hash_hex_is_sixty_four_lowercase_hex_digits() {
        let p = plan_batch(&pairs(&[(b"a", b"x")]), &[name(b"a")], SENSITIVE);
        let hex = p.hash_hex();
        assert_eq!(hex.len(), 64);
        assert!(
            hex.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
    }

    /// The order of the pairs does not change where a swap parks its file: the
    /// temporary is derived from the SORTED intent.
    #[test]
    fn the_temporary_does_not_depend_on_the_order_of_the_pairs() {
        let listing = vec![name(b"a"), name(b"b")];
        let one = plan_batch(&pairs(&[(b"a", b"b"), (b"b", b"a")]), &listing, SENSITIVE);
        let other = plan_batch(&pairs(&[(b"b", b"a"), (b"a", b"b")]), &listing, SENSITIVE);
        let temp = |p: &RenamePlan| {
            p.steps
                .iter()
                .find(|s| s.temp)
                .expect("a temporary")
                .to
                .clone()
        };
        assert_eq!(temp(&one), temp(&other));
    }

    // ---- the twin directory ----------------------------------------------

    /// THE case the two equalities exist for: a directory holding an NFC
    /// `café` AND an NFD `café`, which is what a folder of Mac copies plus one
    /// locally created file looks like on ext4. They are two files. A request
    /// that quotes one of them byte for byte gets THAT one, and the order the
    /// listing happened to arrive in decides nothing.
    ///
    /// Resolving the source through the folded key alone — which is what the
    /// planner used to do — answered "whichever the listing yielded first" and
    /// renamed the wrong file.
    #[test]
    fn an_exact_match_picks_its_twin_whatever_the_listing_order() {
        let nfc = "café".as_bytes().to_vec();
        let nfd = "cafe\u{301}".as_bytes().to_vec();
        for listing in [
            vec![nfc.clone(), nfd.clone()],
            vec![nfd.clone(), nfc.clone()],
        ] {
            for wanted in [&nfc, &nfd] {
                let p = plan_batch(&[(wanted.clone(), name(b"coffee"))], &listing, SENSITIVE);
                assert!(p.executable(), "{:?}", p.collisions);
                assert_eq!(p.steps.len(), 1, "{:?}", p.steps);
                assert_eq!(
                    &p.steps[0].from, wanted,
                    "the twin that was asked for, not the one listed first",
                );
            }
        }
    }

    /// The cleanup `cafe\u{301} → café` in a directory that ALREADY holds an
    /// NFC `café`. The source is one file and the destination is another, so
    /// this is not a null step and not machinery: it is a rename onto an
    /// occupied name, and the only safe answer is a verdict — in BOTH listing
    /// orders.
    ///
    /// Two traps meet here. The old folded source lookup said "nothing to do"
    /// for one order and planned the rename for the other; and a destination
    /// guard that asks "does any pair vacate this KEY" says yes — the source
    /// folds onto its own destination — and clobbers the NFC file. Vacating is
    /// a property of BYTES.
    #[test]
    fn cleaning_a_twin_onto_its_occupied_spelling_is_external_in_both_orders() {
        let nfc = "café".as_bytes().to_vec();
        let nfd = "cafe\u{301}".as_bytes().to_vec();
        for listing in [
            vec![nfc.clone(), nfd.clone()],
            vec![nfd.clone(), nfc.clone()],
        ] {
            let p = plan_batch(&[(nfd.clone(), nfc.clone())], &listing, SENSITIVE);
            assert!(
                !p.executable(),
                "the NFC twin is a file, not a spelling: {:?}",
                p.steps,
            );
            assert_eq!(
                p.collisions,
                vec![Collision {
                    name: nfc.clone(),
                    kind: CollisionKind::External,
                    pair_index: 0,
                }],
            );
        }
    }

    /// Tier three: the request matches nothing byte for byte and folds onto two
    /// entries at once. The planner refuses instead of picking one, in both
    /// listing orders, and names the SOURCE the caller wrote — that is the text
    /// they have to correct.
    #[test]
    fn a_source_that_folds_onto_two_entries_is_ambiguous_in_both_orders() {
        let nfc = "café".as_bytes().to_vec();
        let nfd = "cafe\u{301}".as_bytes().to_vec();
        let cases: [(Vec<u8>, Vec<u8>, Vec<u8>); 2] = [
            // A case-insensitive directory that also holds both normalisations.
            ("CAFÉ".as_bytes().to_vec(), nfc, nfd),
            // A listing that simply contradicts its own capabilities.
            (name(b"FOO"), name(b"Foo"), name(b"foo")),
        ];
        for (asked, one, other) in cases {
            for listing in [
                vec![one.clone(), other.clone()],
                vec![other.clone(), one.clone()],
            ] {
                let p = plan_batch(&[(asked.clone(), name(b"z"))], &listing, INSENSITIVE);
                assert!(!p.executable(), "{:?}", p.steps);
                assert_eq!(
                    p.collisions,
                    vec![Collision {
                        name: asked.clone(),
                        kind: CollisionKind::AmbiguousSource,
                        pair_index: 0,
                    }],
                );
            }
        }
    }

    /// When two twins both block a destination, the verdict names the one
    /// spelled like the request — the user asked for `café` and a `café` really
    /// is there, so pointing at its NFD sibling would be gratuitously
    /// confusing. It has to be the SECOND candidate for this to prove anything:
    /// with the exact one first, the fallback path would answer the same.
    #[test]
    fn the_blocker_reported_is_the_one_spelled_like_the_request() {
        let nfc = "café".as_bytes().to_vec();
        let nfd = "cafe\u{301}".as_bytes().to_vec();
        let p = plan_batch(
            &[(name(b"x"), nfc.clone())],
            &[name(b"x"), nfd, nfc.clone()],
            SENSITIVE,
        );
        assert!(!p.executable());
        assert_eq!(
            p.collisions,
            vec![Collision {
                name: nfc,
                kind: CollisionKind::External,
                pair_index: 0,
            }],
        );
    }

    /// The limit of the escape hatch, pinned so nobody reads the
    /// `AmbiguousSource` docs as a promise it does not make: quoting both twins
    /// byte for byte gets both past source resolution, and then the SECOND one
    /// collects an `Internal`, because collisions are measured folded and the
    /// first pair has already claimed that key. Two batches, not one.
    #[test]
    fn both_twins_in_one_batch_is_internal_however_exactly_they_are_spelled() {
        let nfc = "café".as_bytes().to_vec();
        let nfd = "cafe\u{301}".as_bytes().to_vec();
        let listing = vec![nfc.clone(), nfd.clone()];
        let p = plan_batch(
            &[(nfc, name(b"one")), (nfd, name(b"two"))],
            &listing,
            SENSITIVE,
        );
        assert!(!p.executable(), "{:?}", p.steps);
        assert_eq!(
            p.collisions,
            vec![Collision {
                name: name(b"two"),
                kind: CollisionKind::Internal,
                pair_index: 1,
            }],
        );
    }

    /// A listing that repeats one entry is a paginated `fs.list` racing a
    /// mutation, not a directory with twins. Counting it twice would invent an
    /// `AmbiguousSource` where there is nothing ambiguous at all.
    #[test]
    fn a_repeated_listing_entry_is_not_a_twin() {
        let p = plan_batch(
            &pairs(&[(b"A", b"z")]),
            &[name(b"a"), name(b"a")],
            INSENSITIVE,
        );
        assert!(p.executable(), "{:?}", p.collisions);
        assert_eq!(p.steps.len(), 1);
        assert_eq!(p.steps[0].from, name(b"a"));
    }

    /// Tier two survives the fix: ONE folded candidate and no exact match is
    /// the everyday macOS case — typed NFC, stored NFD — and it still resolves.
    /// Ambiguity is about two candidates, never about one.
    #[test]
    fn a_unique_fold_match_still_resolves() {
        let nfd = "cafe\u{301}".as_bytes().to_vec();
        let p = plan_batch(
            &[("café".as_bytes().to_vec(), name(b"coffee"))],
            std::slice::from_ref(&nfd),
            SENSITIVE,
        );
        assert!(p.executable(), "{:?}", p.collisions);
        assert_eq!(p.steps[0].from, nfd);
    }

    // ---- deliberate choices ----------------------------------------------

    /// Verdicts are ROOT CAUSES and do NOT cascade, and this is the exact input
    /// that shows it: `b→a` is just as unrunnable as `a→z` once `a→z` is
    /// refused, and only `a→z` is reported. Cascading would bury the one line
    /// the user has to act on. Pinned so the choice cannot flip in silence.
    #[test]
    fn a_verdict_does_not_cascade_onto_the_pairs_it_dooms() {
        let p = plan_batch(
            &pairs(&[(b"a", b"z"), (b"b", b"a")]),
            &[name(b"a"), name(b"b"), name(b"z")],
            SENSITIVE,
        );
        assert!(!p.executable());
        assert_eq!(
            p.collisions,
            vec![Collision {
                name: name(b"z"),
                kind: CollisionKind::External,
                pair_index: 0,
            }],
            "only the cause; `b→a` is a consequence and stays silent",
        );
    }

    /// A batch that says both "`a` stays `a`" and "`a` becomes `b`" contradicts
    /// itself, and it does so in BOTH orders. The source claim is registered
    /// before the no-op question, so a dropped identity pair still holds its
    /// source and the later pair loses.
    #[test]
    fn an_identity_pair_still_claims_its_source() {
        let p = plan_batch(
            &pairs(&[(b"a", b"a"), (b"a", b"b")]),
            &[name(b"a")],
            SENSITIVE,
        );
        assert!(!p.executable(), "{:?}", p.steps);
        assert_eq!(
            p.collisions,
            vec![Collision {
                name: name(b"b"),
                kind: CollisionKind::Internal,
                pair_index: 1,
            }],
        );
        // The mirror, which was already caught: the real rename comes first.
        let mirror = plan_batch(
            &pairs(&[(b"a", b"b"), (b"a", b"a")]),
            &[name(b"a")],
            SENSITIVE,
        );
        assert_eq!(mirror.collisions.len(), 1, "{:?}", mirror.collisions);
        assert_eq!(mirror.collisions[0].pair_index, 1);
    }

    /// Every step says which requested pair it descends from, and both halves
    /// of a broken cycle carry the index of the pair whose file took the
    /// detour. This is what lets an executor blame the right ROW when a
    /// `rename` dies mid-batch.
    #[test]
    fn every_step_names_the_pair_it_descends_from() {
        let listing = vec![name(b"a"), name(b"b"), name(b"c")];
        let p = plan_batch(
            &pairs(&[(b"a", b"b"), (b"b", b"a"), (b"c", b"x")]),
            &listing,
            SENSITIVE,
        );
        assert!(p.executable(), "{:?}", p.collisions);
        for s in &p.steps {
            assert!(s.pair_index < 3, "an index into the caller's pairs");
        }
        // The detour's two halves belong to ONE pair: the one parked aside.
        let detour: Vec<u32> = p
            .steps
            .iter()
            .filter(|s| s.temp)
            .map(|s| s.pair_index)
            .collect();
        assert_eq!(detour.len(), 2, "{:?}", p.steps);
        assert_eq!(detour[0], detour[1], "both halves blame the same row");
        // The independent rename keeps its own index.
        let plain: Vec<(u32, Vec<u8>)> = p
            .steps
            .iter()
            .filter(|s| !s.temp)
            .map(|s| (s.pair_index, s.to.clone()))
            .collect();
        assert!(plain.contains(&(2, name(b"x"))), "{plain:?}");
    }

    // ---- properties ------------------------------------------------------

    use proptest::prelude::*;

    /// Apply a plan's steps to a listing and return the resulting names,
    /// sorted. Panics if a step is not applicable, which is itself the check.
    fn simulate(plan: &RenamePlan, listing: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let mut state: Vec<Vec<u8>> = listing.to_vec();
        for s in &plan.steps {
            let idx = state
                .iter()
                .position(|x| x == &s.from)
                .expect("a step renames something that is there");
            assert!(
                !state.iter().any(|x| x == &s.to),
                "a step never clobbers an occupied name",
            );
            state[idx] = s.to.clone();
        }
        state.sort();
        state
    }

    proptest! {
        /// Whatever the pairs, planning TERMINATES and either refuses or
        /// produces steps that, applied in order to the listing, end at exactly
        /// the requested destinations. This is the whole contract in one test.
        #[test]
        fn an_executable_plan_lands_every_destination(
            n in 1usize..6,
            perm in proptest::collection::vec(0usize..6, 1..6),
        ) {
            let listing: Vec<Vec<u8>> =
                (0..n).map(|i| format!("f{i}").into_bytes()).collect();
            let ps: Vec<(Vec<u8>, Vec<u8>)> = listing
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let j = perm[i % perm.len()] % n;
                    (f.clone(), format!("f{j}").into_bytes())
                })
                .collect();
            let plan = plan_batch(&ps, &listing, SENSITIVE);
            if !plan.executable() {
                return Ok(());
            }
            let mut want: Vec<Vec<u8>> = ps.iter().map(|(_, t)| t.clone()).collect();
            want.sort();
            prop_assert_eq!(simulate(&plan, &listing), want);
        }

        /// The same contract when the batch also invents names that are not in
        /// the directory — chains that END somewhere new, which the permutation
        /// generator above can never produce.
        ///
        /// Deliberately case-SENSITIVE only: these names are `f0`…`f11`, where
        /// `name_key` is the identity and the regime flag would change nothing
        /// but the case count. The regimes are varied where it means something,
        /// over `HOSTILE`.
        #[test]
        fn fresh_destinations_land_too(
            n in 1usize..6,
            targets in proptest::collection::vec(0usize..12, 1..6),
        ) {
            let caps = SENSITIVE;
            let listing: Vec<Vec<u8>> =
                (0..n).map(|i| format!("f{i}").into_bytes()).collect();
            // Half the destinations are existing names, half are brand new.
            let ps: Vec<(Vec<u8>, Vec<u8>)> = listing
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let t = targets[i % targets.len()] % 12;
                    (f.clone(), format!("f{t}").into_bytes())
                })
                .collect();
            let plan = plan_batch(&ps, &listing, caps);
            prop_assert!(plan.collisions.is_empty() || plan.steps.is_empty());
            if !plan.executable() {
                return Ok(());
            }
            let mut want: Vec<Vec<u8>> = ps.iter().map(|(_, t)| t.clone()).collect();
            want.sort();
            prop_assert_eq!(simulate(&plan, &listing), want);
        }

        /// Adversarial shapes — arbitrary bytes, sources that do not exist,
        /// repeated names, both case regimes. Nothing here has to SUCCEED; the
        /// planner has to terminate, keep the dead-plan invariant, and never
        /// invent a step whose sides are the same name.
        #[test]
        fn planning_terminates_and_keeps_its_invariants(
            // Any byte EXCEPT what the preconditions rule out: a `/`, a NUL,
            // and the names `.`/`..` are not directory entries at all, and
            // feeding one here would only test the `debug_assert` that says so.
            names in proptest::collection::vec(
                proptest::collection::vec(
                    (1u8..=255).prop_filter("no separator", |b| *b != b'/'), 1..4)
                    .prop_filter("a directory ENTRY, not `.`/`..`", |n: &Vec<u8>| {
                        n.as_slice() != b"." && n.as_slice() != b".."
                    }),
                1..8),
            idx in proptest::collection::vec((0usize..8, 0usize..8), 0..8),
            insensitive in any::<bool>(),
        ) {
            let caps = NameCaps { case_sensitive: !insensitive };
            let listing: Vec<Vec<u8>> = names.clone();
            let ps: Vec<(Vec<u8>, Vec<u8>)> = idx
                .iter()
                .map(|(a, b)| {
                    (names[a % names.len()].clone(), names[b % names.len()].clone())
                })
                .collect();
            let plan = plan_batch(&ps, &listing, caps);
            prop_assert!(plan.collisions.is_empty() || plan.steps.is_empty());
            // Re-planning the same question answers the same thing.
            prop_assert_eq!(&plan, &plan_batch(&ps, &listing, caps));
            for s in &plan.steps {
                prop_assert_ne!(&s.from, &s.to, "a step that does nothing");
            }
            let mut seen: Vec<u32> = plan.collisions.iter().map(|c| c.pair_index).collect();
            let sorted = { let mut s = seen.clone(); s.sort_unstable(); s };
            prop_assert_eq!(&seen, &sorted, "verdicts come back in pair order");
            seen.dedup();
            prop_assert_eq!(seen.len(), plan.collisions.len(), "one verdict per pair");
        }
    }

    /// Six names that are all the SAME name under one rule or another and all
    /// different under some other: NFC and NFD `café`, its uppercase spelling,
    /// `A` and `a`, a capital that folds to two code points, and bytes that are
    /// not UTF-8 at all and must therefore fold to nothing.
    ///
    /// The other properties never reach here. One only ever asks for names that
    /// are already in the listing, another varies `caps` over `f0`…`f5` where
    /// `name_key` is the identity and the flag changes nothing at all, and the
    /// third draws random bytes that essentially never form valid non-NFC
    /// UTF-8. Every bug this module has had lived in this alphabet.
    const HOSTILE: [&[u8]; 7] = [
        "café".as_bytes(),
        "cafe\u{301}".as_bytes(),
        "CAFÉ".as_bytes(),
        b"A",
        b"a",
        "\u{130}".as_bytes(),
        b"\xff\xfe",
    ];

    proptest! {
        /// Both the listing and the destinations come out of [`HOSTILE`], under
        /// both case regimes, with sources that may or may not be spelled the
        /// way the directory spells them.
        ///
        /// Two claims. LANDING: an executable plan applies cleanly — every
        /// `from` is present when its step runs, no step ever writes over an
        /// occupied name (that is `simulate`) — and every destination is there
        /// at the end. ACCOUNTING: no pair vanishes. Each one is renamed, or
        /// carries a verdict, or was a no-op, and a no-op needs its destination
        /// to have been in the directory already.
        #[test]
        fn hostile_names_land_or_are_explained(
            listing_idx in proptest::collection::vec(0usize..7, 1..5),
            pair_idx in proptest::collection::vec((0usize..7, 0usize..7), 1..5),
            insensitive in any::<bool>(),
        ) {
            let caps = NameCaps { case_sensitive: !insensitive };
            // A directory cannot hold one byte string twice.
            let mut listing: Vec<Vec<u8>> = Vec::new();
            for i in listing_idx {
                let n = HOSTILE[i].to_vec();
                if !listing.contains(&n) {
                    listing.push(n);
                }
            }
            let ps: Vec<(Vec<u8>, Vec<u8>)> = pair_idx
                .iter()
                .map(|(a, b)| (HOSTILE[*a].to_vec(), HOSTILE[*b].to_vec()))
                .collect();

            let plan = plan_batch(&ps, &listing, caps);
            prop_assert!(plan.collisions.is_empty() || plan.steps.is_empty());
            prop_assert_eq!(&plan, &plan_batch(&ps, &listing, caps));
            if !plan.executable() {
                // A dead plan says why, it says it about a real pair, and the
                // reason it gives has to be TRUE of the directory. Checking
                // only the shape would let every over-refusal through: the
                // planner would be free to answer `AbsentSource` for a file
                // that is right there.
                prop_assert!(!plan.collisions.is_empty());
                for c in &plan.collisions {
                    let at = usize::try_from(c.pair_index).expect("an index");
                    prop_assert!(at < ps.len());
                    let (from, _) = &ps[at];
                    let key = name_key(from, caps).into_owned();
                    let twins = listing
                        .iter()
                        .filter(|e| name_key(e, caps).as_ref() == key.as_slice())
                        .count();
                    match c.kind {
                        CollisionKind::AbsentSource => prop_assert_eq!(
                            twins, 0, "absent, yet the directory holds it"
                        ),
                        CollisionKind::AmbiguousSource => {
                            prop_assert!(twins >= 2, "ambiguous needs two candidates");
                            prop_assert!(
                                !listing.contains(from),
                                "an exact match is never ambiguous",
                            );
                        }
                        CollisionKind::External => prop_assert!(
                            listing.contains(&c.name),
                            "the blocker has to be a file that exists",
                        ),
                        // `Internal` is about the batch, not the directory, and
                        // the pair-order pin below already covers it.
                        CollisionKind::Internal => prop_assert!(ps.len() > 1),
                    }
                }
                return Ok(());
            }

            let landed = simulate(&plan, &listing);
            for (i, (_, to)) in ps.iter().enumerate() {
                prop_assert!(landed.contains(to), "destination {:?} never landed", to);
                let at = u32::try_from(i).expect("a small batch");
                let renamed = plan.steps.iter().any(|s| s.pair_index == at);
                // The only way out without a step and without a verdict is a
                // null pair, and a null pair renames a file onto its own name.
                prop_assert!(
                    renamed || listing.contains(to),
                    "pair {} vanished: neither a step nor a verdict nor a no-op",
                    i,
                );
            }
        }
    }
}
