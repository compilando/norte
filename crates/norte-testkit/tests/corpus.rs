//! Sanidad del corpus canónico: 25 fixtures (19 nombres + 6 contenidos),
//! nombres válidos como segmentos `VPath`, contenidos con la forma declarada.

use norte_testkit::corpus::{content_fixtures, hostile_names};

#[test]
fn corpus_counts() {
    assert_eq!(hostile_names().len(), 19, "nombres hostiles");
    assert_eq!(content_fixtures().len(), 6, "contenidos legacy");
}

#[test]
fn names_are_valid_segments_and_unique() {
    let names = hostile_names();
    let mut seen = std::collections::HashSet::new();
    for n in &names {
        assert!(
            norte_proto::Segment::new(n.bytes.clone()).is_ok(),
            "[{}] debe ser segmento válido",
            n.id
        );
        assert!(seen.insert(n.bytes.clone()), "[{}] bytes duplicados", n.id);
        assert!(!n.why.is_empty(), "[{}] documenta por qué es hostil", n.id);
    }
}

#[test]
fn names_roundtrip_via_wire() {
    let root = norte_testkit::MemProvider::root();
    for n in hostile_names() {
        let seg = norte_proto::Segment::new(n.bytes.clone()).unwrap();
        let p = root.join(seg);
        let q = norte_proto::VPath::parse(&p.to_wire()).unwrap();
        assert_eq!(
            q.file_name().unwrap().as_bytes(),
            n.bytes.as_slice(),
            "[{}] roundtrip byte-exacto",
            n.id
        );
    }
}

#[test]
fn contents_match_declared_shape() {
    for c in content_fixtures() {
        match c.id {
            "utf16le_bom" => {
                assert_eq!(&c.bytes[..2], &[0xFF, 0xFE], "BOM LE");
                let units: Vec<u16> = c.bytes[2..]
                    .chunks(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect();
                assert_eq!(String::from_utf16(&units).unwrap(), c.decoded);
            }
            "utf16be_bom" => {
                assert_eq!(&c.bytes[..2], &[0xFE, 0xFF], "BOM BE");
                let units: Vec<u16> = c.bytes[2..]
                    .chunks(2)
                    .map(|b| u16::from_be_bytes([b[0], b[1]]))
                    .collect();
                assert_eq!(String::from_utf16(&units).unwrap(), c.decoded);
            }
            "windows_1252_curly" => {
                assert_eq!(c.bytes, b"it\x92s\n".to_vec());
                assert_eq!(c.decoded, "it\u{2019}s\n");
                // Un decoder ISO-8859-1 estricto daría U+0092 (control), no ’.
                assert_ne!(char::from(0x92u8), '\u{2019}');
            }
            "latin1" => {
                assert!(
                    std::str::from_utf8(&c.bytes).is_err(),
                    "latin1 con ñ NO es UTF-8 válido"
                );
                // Decodificación Latin-1 manual: byte → codepoint.
                let decoded: String = c.bytes.iter().map(|&b| char::from(b)).collect();
                assert_eq!(decoded, c.decoded);
            }
            "shift_jis" => {
                assert!(
                    std::str::from_utf8(&c.bytes).is_err(),
                    "Shift-JIS multibyte NO es UTF-8 válido"
                );
                assert_eq!(c.decoded, "テスト\n");
            }
            "utf8_bom" => {
                assert_eq!(&c.bytes[..3], &[0xEF, 0xBB, 0xBF], "BOM UTF-8");
                assert_eq!(std::str::from_utf8(&c.bytes[3..]).unwrap(), c.decoded);
            }
            other => panic!("fixture inesperada: {other}"),
        }
    }
}
