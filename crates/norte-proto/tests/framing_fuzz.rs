//! Phase 10c — fuzz of the NDJSON framing decoder (ADR 0011): adversarial or
//! random streams must NOT panic or OOM, every frame comes out clean, and
//! chopping the stream up across `push()` calls does not change the frames
//! produced.

use norte_proto::wire::{FrameDecoder, FrameOversized, MAX_FRAME_BYTES};
use proptest::prelude::*;

/// Drains every available frame.
fn drain(d: &mut FrameDecoder) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(f) = d.next_frame() {
        out.push(f);
    }
    out
}

proptest! {
    /// Chopping the stream up across `push()` calls is IRRELEVANT: the same
    /// bytes, fed whole or split at arbitrary points, produce the SAME
    /// sequence of frames. (Size < MAX_FRAME_BYTES so as not to cross the
    /// oversize path, which does depend on the chopping by design.)
    #[test]
    fn chunk_boundary_independence(
        data in proptest::collection::vec(any::<u8>(), 0..4096),
        raw_splits in proptest::collection::vec(any::<usize>(), 0..16),
    ) {
        // Whole.
        let mut d1 = FrameDecoder::new();
        d1.push(&data).expect("under MAX is never oversized");
        let whole = drain(&mut d1);

        // Split at arbitrary points (sorted and bounded to the range).
        let mut points: Vec<usize> =
            raw_splits.iter().map(|s| s % (data.len() + 1)).collect();
        points.sort_unstable();
        let mut d2 = FrameDecoder::new();
        let mut idx = 0;
        for &p in &points {
            d2.push(&data[idx..p]).expect("under MAX");
            idx = p;
        }
        d2.push(&data[idx..]).expect("under MAX");
        let split = drain(&mut d2);

        prop_assert_eq!(&whole, &split, "chopping it up changed the frames");

        // No emitted frame carries `\n` or is empty (empty lines are dropped
        // as keepalive).
        for f in &whole {
            prop_assert!(!f.contains(&b'\n'), "a frame cannot contain `\\n`");
            prop_assert!(!f.is_empty(), "an empty frame is never emitted");
        }
    }

    /// The decoder RESYNCS on `\n`: after any garbage before it, a `\n`
    /// closes the partial leftover and the next well-formed frame comes out
    /// clean. (The leading `\n` is essential: without it, an unclosed
    /// leftover would prefix the frame — correct concatenation behavior.)
    #[test]
    fn resyncs_on_newline_after_arbitrary_input(
        first in proptest::collection::vec(any::<u8>(), 0..2048),
    ) {
        let mut d = FrameDecoder::new();
        d.push(&first).expect("under MAX");
        let _ = drain(&mut d);
        // `\n` closes the leftover, then a clean frame.
        d.push(b"\nafter\n").expect("under MAX");
        let rest = drain(&mut d);
        prop_assert!(
            rest.iter().any(|f| f == b"after"),
            "after a `\\n` the decoder emits the clean frame: {rest:?}"
        );
    }
}

#[test]
fn oversize_unterminated_frame_errors() {
    // 16 MiB + 1 WITHOUT `\n` → FrameOversized (does not accumulate forever →
    // no OOM).
    let mut d = FrameDecoder::new();
    let big = vec![b'x'; MAX_FRAME_BYTES + 1];
    assert!(matches!(d.push(&big), Err(FrameOversized)));
}

#[test]
fn terminated_frame_over_max_is_accepted_by_design() {
    // A frame that exceeds MAX but CARRIES a `\n` is not oversized (the `\n`
    // bounds it): documents ADR 0011's semantics ("oversized = without `\n`").
    let mut d = FrameDecoder::new();
    let mut big = vec![b'y'; MAX_FRAME_BYTES + 10];
    big.push(b'\n');
    assert!(d.push(&big).is_ok());
    assert!(d.next_frame().is_some());
}
