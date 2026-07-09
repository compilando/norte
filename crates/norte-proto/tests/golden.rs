//! Golden tests del wire format (spec §12): las fixtures JSON commiteadas son
//! el contrato. Si un cambio de código rompe uno de estos tests, es un cambio
//! de wire format: exige bump de versión de protocolo y revisión doble.

use norte_proto::{Authority, Scheme, Segment, VPath};
use serde_json::Value;
use std::path::Path;

fn load(name: &str) -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("no se pudo leer {}: {e}", path.display()));
    serde_json::from_str(&raw).expect("fixture JSON válida")
}

fn hex_decode(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "hex de longitud par: {s}");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("dígitos hex"))
        .collect()
}

fn segments_hex(entry: &Value) -> Vec<Vec<u8>> {
    entry["segments_hex"]
        .as_array()
        .expect("segments_hex es array")
        .iter()
        .map(|h| hex_decode(h.as_str().expect("hex string")))
        .collect()
}

fn build(scheme: &str, authority: Option<&str>, segments: &[Vec<u8>]) -> VPath {
    let mut p = VPath::root(
        Scheme::new(scheme).expect("scheme de fixture válido"),
        authority.map(|a| Authority::new(a).expect("authority de fixture válida")),
    );
    for s in segments {
        p = p.join(Segment::new(s.clone()).expect("segmento de fixture válido"));
    }
    p
}

#[test]
fn golden_vpath_valid() {
    for entry in load("vpath/valid.json") {
        let wire = entry["wire"].as_str().unwrap();
        let p = VPath::parse(wire)
            .unwrap_or_else(|e| panic!("[{wire}] debía parsear, falló con {e:?}"));

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
        assert_eq!(actual, expected, "[{wire}] segmentos byte-exactos");

        let canonical = entry["canonical_wire"].as_str().unwrap();
        assert_eq!(p.to_wire(), canonical, "[{wire}] forma canónica");
        assert_eq!(
            VPath::parse(canonical).unwrap(),
            p,
            "[{wire}] la forma canónica re-parsea igual"
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
            Ok(p) => panic!("[{wire}] debía fallar con {kind}, parseó a {p:?}"),
            Err(e) => assert_eq!(format!("{e:?}"), kind, "[{wire}] variante de error"),
        }
    }
}

#[test]
fn golden_vpath_hostile_corpus() {
    for entry in load("vpath/hostile_corpus.json") {
        let name = entry["name"].as_str().unwrap();
        let segments = segments_hex(&entry);
        let expected_wire = entry["expected_wire"].as_str().unwrap();

        // Dirección encode: desde bytes, el encoder queda fijado byte a byte.
        let p = build("file", None, &segments);
        assert_eq!(p.to_wire(), expected_wire, "[{name}] encode");

        // Vuelta: el wire producido decodifica a los mismos bytes.
        let q = VPath::parse(expected_wire)
            .unwrap_or_else(|e| panic!("[{name}] su wire debía parsear: {e:?}"));
        let back: Vec<Vec<u8>> = q.segments().map(<[u8]>::to_vec).collect();
        assert_eq!(back, segments, "[{name}] roundtrip byte-exacto");

        assert_eq!(
            p.display_lossy(),
            entry["display"].as_str().unwrap(),
            "[{name}] display"
        );
    }
}
