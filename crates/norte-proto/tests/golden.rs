//! Golden tests of the wire format (spec §12): the committed JSON fixtures
//! are the contract. If a code change breaks one of these tests, it is a wire
//! format change: it requires a protocol version bump and a double review.

use norte_proto::{Authority, Scheme, Segment, VPath};
use serde_json::Value;
use std::path::Path;

fn load(name: &str) -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    serde_json::from_str(&raw).expect("valid JSON fixture")
}

fn hex_decode(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "even-length hex: {s}");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex digits"))
        .collect()
}

fn segments_hex(entry: &Value) -> Vec<Vec<u8>> {
    entry["segments_hex"]
        .as_array()
        .expect("segments_hex is an array")
        .iter()
        .map(|h| hex_decode(h.as_str().expect("hex string")))
        .collect()
}

fn build(scheme: &str, authority: Option<&str>, segments: &[Vec<u8>]) -> VPath {
    let mut p = VPath::root(
        Scheme::new(scheme).expect("valid fixture scheme"),
        authority.map(|a| Authority::new(a).expect("valid fixture authority")),
    );
    for s in segments {
        p = p.join(Segment::new(s.clone()).expect("valid fixture segment"));
    }
    p
}

#[test]
fn golden_vpath_valid() {
    for entry in load("vpath/valid.json") {
        let wire = entry["wire"].as_str().unwrap();
        let p = VPath::parse(wire)
            .unwrap_or_else(|e| panic!("[{wire}] should have parsed, failed with {e:?}"));

        assert_eq!(
            p.scheme(),
            entry["scheme"].as_str().unwrap(),
            "[{wire}] scheme"
        );
        assert_eq!(
            p.authority(),
            entry["authority"].as_str(),
            "[{wire}] authority"
        );

        let expected = segments_hex(&entry);
        let actual: Vec<Vec<u8>> = p.segments().map(<[u8]>::to_vec).collect();
        assert_eq!(actual, expected, "[{wire}] byte-exact segments");

        let canonical = entry["canonical_wire"].as_str().unwrap();
        assert_eq!(p.to_wire(), canonical, "[{wire}] canonical form");
        assert_eq!(
            VPath::parse(canonical).unwrap(),
            p,
            "[{wire}] the canonical form re-parses the same"
        );
        assert_eq!(
            p.display_lossy(),
            entry["display"].as_str().unwrap(),
            "[{wire}] display"
        );
    }
}

#[test]
fn golden_vpath_invalid() {
    for entry in load("vpath/invalid.json") {
        let wire = entry["wire"].as_str().unwrap();
        let kind = entry["error_kind"].as_str().unwrap();
        match VPath::parse(wire) {
            Ok(p) => panic!("[{wire}] should have failed with {kind}, parsed to {p:?}"),
            Err(e) => assert_eq!(format!("{e:?}"), kind, "[{wire}] error variant"),
        }
    }
}

#[test]
fn golden_vpath_hostile_corpus() {
    for entry in load("vpath/hostile_corpus.json") {
        let name = entry["name"].as_str().unwrap();
        let segments = segments_hex(&entry);
        let expected_wire = entry["expected_wire"].as_str().unwrap();

        // Encode direction: starting from bytes, the encoder is pinned byte
        // by byte.
        let p = build("file", None, &segments);
        assert_eq!(p.to_wire(), expected_wire, "[{name}] encode");

        // Return trip: the produced wire decodes back to the same bytes.
        let q = VPath::parse(expected_wire)
            .unwrap_or_else(|e| panic!("[{name}] its wire should have parsed: {e:?}"));
        let back: Vec<Vec<u8>> = q.segments().map(<[u8]>::to_vec).collect();
        assert_eq!(back, segments, "[{name}] byte-exact roundtrip");

        assert_eq!(
            p.display_lossy(),
            entry["display"].as_str().unwrap(),
            "[{name}] display"
        );
    }
}
