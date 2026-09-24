//! Canonical proptest strategies (spec §12): published by EVERY crate that
//! does property-based testing over paths. The local copies in
//! `norte-proto/tests` (which cannot depend on this crate: a cycle) must be
//! kept aligned with these.

use norte_proto::{Authority, Scheme, Segment, VPath};
use proptest::prelude::*;

use crate::corpus;

/// Valid segment bytes: 1–64 bytes, no NUL, no `/`, no `.`/`..`.
pub fn arb_segment_bytes() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 1..64)
        .prop_filter("no NUL or separator", |b| {
            !b.contains(&0x00) && !b.contains(&0x2F)
        })
        .prop_filter("no dot-segments", |b| b != b"." && b != b"..")
}

/// Valid schemes (`[a-z][a-z0-9+.-]*`).
///
/// # Panics
/// Never: the regex is constant and valid.
pub fn arb_scheme() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z][a-z0-9+.-]{0,10}").expect("valid regex")
}

/// Valid or absent authorities (full charset: printable ASCII without `%` or
/// `/`).
///
/// # Panics
/// Never: the regex is constant and only generates valid authorities.
pub fn arb_authority() -> impl Strategy<Value = Option<Authority>> {
    let valid = proptest::string::string_regex("[!-$&-.0-~]{1,16}").expect("valid regex");
    proptest::option::of(
        valid.prop_map(|s| Authority::new(&s).expect("the strategy generates valid authorities")),
    )
}

/// Arbitrary `VPath`s: scheme + optional authority + 0–8 byte segments.
///
/// # Panics
/// Never: it composes strategies that only generate valid components.
pub fn arb_vpath() -> impl Strategy<Value = VPath> {
    (
        arb_scheme(),
        arb_authority(),
        proptest::collection::vec(arb_segment_bytes(), 0..8),
    )
        .prop_map(|(scheme, authority, segs)| {
            let scheme = Scheme::new(&scheme).expect("the strategy generates valid schemes");
            let mut p = VPath::root(scheme, authority);
            for s in segs {
                p = p.join(Segment::new(s).expect("the strategy generates valid segments"));
            }
            p
        })
}

/// Hostile filenames: 50% a case from the canonical corpus, 50% arbitrary
/// bytes valid as a segment. Biased toward what breaks real software.
///
/// Watch out with `MemProvider`'s case-insensitivity: its fold is ASCII, and
/// trailing bytes of legacy multibyte encodings (Shift-JIS) can produce a
/// spurious `CaseCollision` — do not assert Unicode collision semantics
/// against the simulator.
///
/// # Panics
/// Never: the embedded corpus is not empty.
pub fn arb_hostile_filename() -> impl Strategy<Value = Vec<u8>> {
    let from_corpus: Vec<Vec<u8>> = corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        .collect();
    prop_oneof![proptest::sample::select(from_corpus), arb_segment_bytes(),]
}
