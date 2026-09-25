//! Short fuzz (proptest) of the framing + JSON-RPC envelope (spec §12, ADR
//! 0011). No sockets: `FrameDecoder` and the envelope parse are I/O-free, so
//! they are fuzzed with plain bytes. Runs on the PR gate (like the TUI's
//! `config_fuzz`); nightly widens the cases.

use norte_proto::wire::{
    FrameDecoder, JsonRpcVersion, Message, MessageKind, Notification, Request, RequestId, classify,
    encode_frame,
};
use proptest::prelude::*;

/// Drains every frame the decoder has ready.
fn drain(dec: &mut FrameDecoder) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(f) = dec.next_frame() {
        out.push(f);
    }
    out
}

proptest! {
    /// No arbitrary byte makes the decoder `panic`, and every emitted frame
    /// is CLEAN: no interior `\n` and no trailing `\r` (NDJSON's contract).
    #[test]
    fn framing_never_panics_frames_are_clean(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let mut dec = FrameDecoder::new();
        // Small inputs: they never come close to MAX_FRAME_BYTES, push cannot fail.
        prop_assert!(dec.push(&data).is_ok());
        for frame in drain(&mut dec) {
            prop_assert!(!frame.contains(&b'\n'), "frame with an interior \\n");
            prop_assert_ne!(frame.last(), Some(&b'\r'), "frame with an untrimmed trailing \\r");
            prop_assert!(!frame.is_empty(), "an empty line should have been dropped");
        }
    }

    /// Chunk boundary invariant: feeding the SAME bytes chopped up any way
    /// delivers EXACTLY the same frames as a single push. This is the
    /// property the `scanned`/`pending_newlines` optimization could break.
    #[test]
    fn framing_chunk_boundary_invariance(
        data in proptest::collection::vec(any::<u8>(), 0..2048),
        cuts in proptest::collection::vec(any::<prop::sample::Index>(), 0..16),
    ) {
        // Reference: a single push.
        let mut whole = FrameDecoder::new();
        prop_assert!(whole.push(&data).is_ok());
        let expected = drain(&mut whole);

        // Chop `data` at arbitrary sorted points.
        let mut points: Vec<usize> = cuts.iter().map(|c| c.index(data.len() + 1)).collect();
        points.sort_unstable();
        let mut chunked = FrameDecoder::new();
        let mut got = Vec::new();
        let mut start = 0;
        for &p in points.iter().chain(std::iter::once(&data.len())) {
            let end = p.max(start).min(data.len());
            prop_assert!(chunked.push(&data[start..end]).is_ok());
            got.extend(drain(&mut chunked));
            start = end;
        }
        prop_assert_eq!(expected, got);
    }

    /// `classify` and `Message`'s parse never `panic` on garbage: the server
    /// must be able to tell "broken JSON" apart from "invalid envelope"
    /// without falling over (spec §12).
    #[test]
    fn envelope_parse_never_panics(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&data) {
            let _ = classify(&v); // no panic
        }
        // Direct typed parse: Ok or Err, never panic.
        let _ = serde_json::from_slice::<Message>(&data);
    }

    /// Roundtrip: a valid request/notification survives encode_frame →
    /// FrameDecoder → parse, with method/id intact. `method` is arbitrary
    /// (includes Unicode and control characters except `\0`, which
    /// serde_json escapes).
    #[test]
    fn envelope_request_roundtrips(
        method in "\\PC{0,64}",
        id in any::<u64>(),
        is_request in any::<bool>(),
    ) {
        let frame = if is_request {
            encode_frame(&Request {
                jsonrpc: JsonRpcVersion,
                id: RequestId::Num(id),
                method: method.clone(),
                params: None,
            })
        } else {
            encode_frame(&Notification {
                jsonrpc: JsonRpcVersion,
                method: method.clone(),
                params: None,
            })
        }
        .expect("a protocol type always serializes");

        let mut dec = FrameDecoder::new();
        prop_assert!(dec.push(&frame).is_ok());
        let bytes = dec.next_frame().expect("the frame closed with \\n");
        let msg: Message = serde_json::from_slice(&bytes).expect("re-parses");

        match (is_request, &msg) {
            (true, Message::Request(r)) => {
                prop_assert_eq!(&r.method, &method);
                prop_assert_eq!(&r.id, &RequestId::Num(id));
            }
            (false, Message::Notification(n)) => prop_assert_eq!(&n.method, &method),
            other => prop_assert!(false, "unexpected classification: {:?}", other),
        }
        // Consistency with classify over the Value.
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let want = if is_request { MessageKind::Request } else { MessageKind::Notification };
        prop_assert_eq!(classify(&v), want);
    }
}
