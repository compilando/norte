//! Property-based tests of `VPath` (spec §12): the byte-exact roundtrip is
//! the project's foundational property — a path is never corrupted.
//!
//! The canonical strategies will migrate to `norte-testkit::strategies` in
//! the testkit phase; the original definitions live here.

use norte_proto::{Authority, Scheme, Segment, VPath};
use proptest::prelude::*;

fn arb_segment_bytes() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 1..64)
        .prop_filter("no NUL nor separator", |b| {
            !b.contains(&0x00) && !b.contains(&0x2F)
        })
        .prop_filter("no dot-segments", |b| b != b"." && b != b"..")
}

fn arb_scheme() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z][a-z0-9+.-]{0,10}").expect("valid regex")
}

fn arb_authority() -> impl Strategy<Value = Option<Authority>> {
    // Authority's charset: printable ASCII minus `%` (0x25) and `/` (0x2F).
    // The charset includes `:` and `@`, so a `user:pass@host` can fall out and
    // is NOT valid anymore (#46, proto 0.8.0): it is FILTERED instead of
    // `expect`ed.
    let valid = proptest::string::string_regex("[!-$&-.0-~]{1,16}").expect("valid regex");
    proptest::option::of(valid.prop_filter_map("valid authority", |s| Authority::new(&s).ok()))
}

prop_compose! {
    fn arb_vpath()(
        scheme in arb_scheme(),
        authority in arb_authority(),
        segs in proptest::collection::vec(arb_segment_bytes(), 0..8),
    ) -> VPath {
        let scheme = Scheme::new(&scheme).expect("the strategy generates valid schemes");
        let mut p = VPath::root(scheme, authority);
        for s in segs {
            p = p.join(Segment::new(s).expect("the strategy generates valid segments"));
        }
        p
    }
}

proptest! {
    /// THE critical property: arbitrary bytes → wire → parse → identical bytes.
    #[test]
    fn prop_roundtrip_bytes(p in arb_vpath()) {
        let wire = p.to_wire();
        let q = VPath::parse(&wire).expect("to_wire always produces parseable wire");
        let a: Vec<&[u8]> = p.segments().collect();
        let b: Vec<&[u8]> = q.segments().collect();
        prop_assert_eq!(a, b);
        prop_assert_eq!(p.scheme(), q.scheme());
        prop_assert_eq!(p.authority(), q.authority());
    }

    /// parse never panics, with any garbage.
    #[test]
    fn prop_parse_never_panics(s in ".*") {
        let _ = VPath::parse(&s);
    }

    /// Garbage biased toward the grammar (truncated escapes, double slashes…).
    #[test]
    fn prop_parse_never_panics_grammarlike(
        s in "[a-zA-Z0-9]{0,4}(://)?[a-z0-9%/.]{0,30}%?[0-9A-Fa-f]?"
    ) {
        let _ = VPath::parse(&s);
    }

    /// Every Ok parse satisfies the segment invariants.
    #[test]
    fn prop_parse_ok_implies_invariants(s in ".*") {
        if let Ok(p) = VPath::parse(&s) {
            for seg in p.segments() {
                prop_assert!(!seg.is_empty());
                prop_assert!(!seg.contains(&0x00));
                prop_assert!(!seg.contains(&0x2F));
                prop_assert!(seg != b"." && seg != b"..");
            }
        }
    }

    /// Idempotent reparse: to_wire is a fixed point after a parse.
    #[test]
    fn prop_reparse_idempotent(s in ".*") {
        if let Ok(p) = VPath::parse(&s) {
            let w1 = p.to_wire();
            let q = VPath::parse(&w1).expect("canonical wire parses");
            prop_assert_eq!(&p, &q);
            prop_assert_eq!(w1, q.to_wire());
        }
    }

    /// The wire is injective: equal wires ⇒ equal paths.
    #[test]
    fn prop_wire_injective(p1 in arb_vpath(), p2 in arb_vpath()) {
        if p1.to_wire() == p2.to_wire() {
            prop_assert_eq!(p1, p2);
        }
    }

    /// join/parent/file_name are consistent.
    #[test]
    fn prop_join_parent_inverse(p in arb_vpath(), s in arb_segment_bytes()) {
        let seg = Segment::new(s).expect("valid segment");
        let child = p.join(seg.clone());
        prop_assert_eq!(child.parent(), Some(p));
        prop_assert_eq!(child.file_name(), Some(&seg));
    }

    /// Serde roundtrip through real JSON.
    #[test]
    fn prop_serde_roundtrip(p in arb_vpath()) {
        let json = serde_json::to_string(&p).expect("serializable");
        let q: VPath = serde_json::from_str(&json).expect("deserializable");
        prop_assert_eq!(p, q);
    }

    /// `Segment`'s serde roundtrip through real JSON — the property analogous
    /// to `prop_serde_roundtrip` but for the ONE-component type.
    #[test]
    fn prop_segment_serde_roundtrip(bytes in arb_segment_bytes()) {
        let s = Segment::new(bytes).expect("the strategy generates valid segments");
        let json = serde_json::to_string(&s).expect("serializable");
        let q: Segment = serde_json::from_str(&json).expect("deserializable");
        prop_assert_eq!(s, q);
    }

    /// A `Segment`'s wire never carries raw controls (terminal injection in
    /// logs) — analogous to `prop_wire_no_raw_controls` for `VPath`.
    #[test]
    fn prop_segment_wire_no_raw_controls(bytes in arb_segment_bytes()) {
        let s = Segment::new(bytes).expect("the strategy generates valid segments");
        prop_assert!(!s.to_wire().chars().any(|c| c.is_ascii_control()));
    }

    /// display_lossy always terminates; clean UTF-8 segments (no legitimate
    /// U+FFFD nor controls) introduce no `�`.
    #[test]
    fn prop_display_never_panics(p in arb_vpath()) {
        let d = p.display_lossy();
        let all_clean_utf8 = p.segments().all(|s| {
            std::str::from_utf8(s).is_ok_and(|t| {
                !t.contains(char::REPLACEMENT_CHARACTER) && !t.chars().any(char::is_control)
            })
        });
        if all_clean_utf8 {
            let clean = !d.contains(char::REPLACEMENT_CHARACTER);
            prop_assert!(clean);
        }
    }

    /// The wire never carries raw controls (terminal injection in logs).
    #[test]
    fn prop_wire_no_raw_controls(p in arb_vpath()) {
        prop_assert!(!p.to_wire().chars().any(|c| c.is_ascii_control()));
    }

    /// The display is never a valid wire: a path can never be reconstructed from it.
    #[test]
    fn prop_display_never_reparses(p in arb_vpath()) {
        prop_assert!(VPath::parse(&p.display_lossy()).is_err());
    }

    /// The display never emits control characters, wherever the bytes came from.
    #[test]
    fn prop_display_no_controls(p in arb_vpath()) {
        prop_assert!(!p.display_lossy().chars().any(char::is_control));
    }
}

/// `VPath`s plausible as the OUTER part of an archive (ADR 0018): scheme with
/// no format prefix, ≥1 segment, no `!` segments.
fn arb_archive_outer() -> impl Strategy<Value = VPath> {
    arb_vpath().prop_filter("composable outer", |p| {
        !p.is_root()
            && p.archive_split().is_ok_and(|r| r.is_none())
            && p.segments().all(|s| s != b"!")
    })
}

proptest! {
    /// `archive_compose`'s rustdoc's "guaranteed roundtrip":
    /// split(compose(f, outer, inner)) == (f, outer, inner).
    #[test]
    fn prop_archive_compose_split_roundtrip(
        outer in arb_archive_outer(),
        inner_bytes in proptest::collection::vec(arb_segment_bytes(), 0..6),
        format in proptest::sample::select(norte_proto::ARCHIVE_FORMATS),
    ) {
        // `outer` and `format` are generated independently: with compound
        // tokens like `tar+gz` in the whitelist, some combinations (e.g.
        // format="tar" over an outer with scheme "gz+something") would form
        // an ambiguous scheme `archive_compose` rejects on purpose (see
        // `archive_compose_rechaza_roundtrip_ambiguo_con_tar_gz` in
        // `tests/vpath.rs`, ADR 0028) — not representative of the roundtrip
        // this property exercises, so the combination is discarded instead of
        // manufacturing a spurious `.expect()` panic.
        prop_assume!(
            norte_proto::scheme_archive_format(&format!("{format}+{}", outer.scheme()))
                == Some(format)
        );
        let inner: Vec<Segment> = inner_bytes
            .into_iter()
            .filter(|b| b.as_slice() != b"!")
            .map(|b| Segment::new(b).expect("valid strategy"))
            .collect();
        let p = VPath::archive_compose(format, &outer, &inner).expect("valid compose");
        let r = p.archive_split().expect("well-formed").expect("compound");
        prop_assert_eq!(r.format.as_str(), format);
        prop_assert_eq!(r.outer, outer);
        prop_assert_eq!(r.inner, inner);
        // And the compound's wire re-parses to the same path (transitivity
        // with prop_reparse_idempotent).
        prop_assert_eq!(VPath::parse(&p.to_wire()).expect("valid wire"), p);
    }

    /// #56: roundtrip across LAYERS — composing a second layer over a
    /// well-formed archive path and peeling it returns exactly what was
    /// composed, and the layer below stays intact.
    #[test]
    fn prop_archive_nested_two_layers_roundtrip(
        outer in arb_archive_outer(),
        inner1_bytes in proptest::collection::vec(arb_segment_bytes(), 1..4),
        inner2_bytes in proptest::collection::vec(arb_segment_bytes(), 0..4),
        f1 in proptest::sample::select(norte_proto::ARCHIVE_FORMATS),
        f2 in proptest::sample::select(norte_proto::ARCHIVE_FORMATS),
    ) {
        prop_assume!(
            norte_proto::scheme_archive_format(&format!("{f1}+{}", outer.scheme()))
                == Some(f1)
        );
        // Layer 2 is prepended to the ALREADY composed scheme: same
        // ambiguity guard (e.g. f2="tar" over "gz+…" would resolve to
        // "tar+gz").
        prop_assume!(
            norte_proto::scheme_archive_format(&format!("{f2}+{f1}+{}", outer.scheme()))
                == Some(f2)
        );
        let seg_ok = |b: &Vec<u8>| b.as_slice() != b"!";
        let inner1: Vec<Segment> = inner1_bytes.into_iter().filter(seg_ok)
            .map(|b| Segment::new(b).expect("valid strategy")).collect();
        let inner2: Vec<Segment> = inner2_bytes.into_iter().filter(seg_ok)
            .map(|b| Segment::new(b).expect("valid strategy")).collect();
        prop_assume!(!inner1.is_empty()); // layer 1 names a real container
        let layer1 = VPath::archive_compose(f1, &outer, &inner1).expect("layer 1");
        let layer2 = VPath::archive_compose(f2, &layer1, &inner2).expect("layer 2");
        let r = layer2.archive_split().expect("well-formed").expect("compound");
        prop_assert_eq!(r.format.as_str(), f2);
        prop_assert_eq!(&r.outer, &layer1);
        prop_assert_eq!(r.inner, inner2);
        let r1 = r.outer.archive_split().expect("well-formed").expect("compound");
        prop_assert_eq!(r1.format.as_str(), f1);
        prop_assert_eq!(r1.outer, outer);
        prop_assert_eq!(r1.inner, inner1);
        prop_assert_eq!(VPath::parse(&layer2.to_wire()).expect("valid wire"), layer2);
    }
}
