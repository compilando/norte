//! Sanity of the canonical corpus: at least 62 fixtures (48 names + 11+3
//! contents), names valid as `VPath` segments, contents with the declared
//! shape.

use norte_testkit::corpus::{content_fixtures, hostile_chords, hostile_names, spelling_twins};

#[test]
fn corpus_counts() {
    // Floor, not an exact count (#169): this and `hostile_names`'s doctest
    // used to assert `== 48` each, and went red at different times because
    // `nextest` does not run doctests — adding a fixture left the doctest
    // red without `just t` ever seeing it. A floor still catches "the
    // corpus got emptied by accident" without growing it costing two
    // touched places.
    assert!(hostile_names().len() >= 48, "hostile names");
    assert_eq!(content_fixtures().len(), 11, "detectable contents");
    assert_eq!(
        norte_testkit::corpus::content_fixtures_forced().len(),
        3,
        "forced-only contents"
    );
    assert_eq!(hostile_chords().len(), 4, "hostile chords (H1)");
}

#[test]
fn hostile_chords_are_a_single_codepoint_hazard_and_unique() {
    let chords = hostile_chords();
    let mut seen = std::collections::HashSet::new();
    for c in &chords {
        assert!(
            norte_encoding::is_terminal_hazard(c.token),
            "[{}] must be a terminal hazard",
            c.id
        );
        assert!(!c.why.is_empty(), "[{}] documents why it is hostile", c.id);
        assert!(seen.insert(c.token), "[{}] duplicate token", c.id);
    }
}

#[test]
fn names_are_valid_segments_and_unique() {
    let names = hostile_names();
    let mut seen = std::collections::HashSet::new();
    for n in &names {
        assert!(
            norte_proto::Segment::new(n.bytes.clone()).is_ok(),
            "[{}] must be a valid segment",
            n.id
        );
        assert!(seen.insert(n.bytes.clone()), "[{}] duplicate bytes", n.id);
        assert!(!n.why.is_empty(), "[{}] documents why it is hostile", n.id);
    }
}

/// #169: the four fixtures requested by `2026-08-11-directory-sync.md`'s
/// three deferrals, pinning the specific property each one claims to have
/// and not just that it exists.
#[test]
fn new_fixtures_from_169_deliver_what_they_promise() {
    let names = hostile_names();
    let find = |id: &str| {
        names
            .iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("fixture {id} in the corpus"))
    };

    // `!`: the archive-composition marker (ADR 0018) as a real name, byte
    // for byte and nothing else.
    assert_eq!(find("archive_marker_literal").bytes, b"!");

    // 255 bytes (NAME_MAX) that are NOT valid UTF-8, unlike `name_max_255`
    // (pure ASCII).
    let tail = find("name_max_255_invalid_tail");
    assert_eq!(tail.bytes.len(), 255);
    assert!(std::str::from_utf8(&tail.bytes).is_err());

    // A name that IS a whole `.trashinfo` sidecar, without the slash
    // `Segment` forbids (the real `Path` goes url-encoded).
    let spoof = find("trashinfo_record_spoof");
    assert!(!spoof.bytes.contains(&b'/'));
    let text = std::str::from_utf8(&spoof.bytes).expect("valid UTF-8");
    assert!(text.contains("Path="));
    assert!(text.contains("DeletionDate="));

    // U+0130, whose full fold is TWO codepoints ('i' + U+0307), not a plain
    // 'i'.
    let turkish = find("turkish_dotted_i_capital");
    let text = std::str::from_utf8(&turkish.bytes).expect("valid UTF-8");
    assert!(text.starts_with('\u{0130}'));
}

/// #169: `spelling_twins()` references corpus `id`s, not its own bytes —
/// both sides of each pair have to really exist and be different.
#[test]
fn spelling_twins_reference_real_and_distinct_ids() {
    let names = hostile_names();
    let ids: std::collections::HashSet<&str> = names.iter().map(|n| n.id.as_str()).collect();
    for twin in spelling_twins() {
        assert!(
            ids.contains(twin.left),
            "[{}] is not in the corpus",
            twin.left
        );
        assert!(
            ids.contains(twin.right),
            "[{}] is not in the corpus",
            twin.right
        );
        assert_ne!(twin.left, twin.right, "a pair is not a name with itself");
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
            "[{}] byte-exact roundtrip",
            n.id
        );
    }
}

#[test]
fn contents_match_declared_shape() {
    for c in content_fixtures() {
        match c.id {
            "utf16le_bom" => {
                assert_eq!(&c.bytes[..2], &[0xFF, 0xFE], "LE BOM");
                let units: Vec<u16> = c.bytes[2..]
                    .chunks(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect();
                assert_eq!(String::from_utf16(&units).unwrap(), c.decoded);
            }
            "utf16be_bom" => {
                assert_eq!(&c.bytes[..2], &[0xFE, 0xFF], "BE BOM");
                let units: Vec<u16> = c.bytes[2..]
                    .chunks(2)
                    .map(|b| u16::from_be_bytes([b[0], b[1]]))
                    .collect();
                assert_eq!(String::from_utf16(&units).unwrap(), c.decoded);
            }
            "windows_1252_curly" => {
                assert_eq!(c.bytes, b"it\x92s\n".to_vec());
                assert_eq!(c.decoded, "it\u{2019}s\n");
                // A strict ISO-8859-1 decoder would give U+0092 (control), not ’.
                assert_ne!(char::from(0x92u8), '\u{2019}');
            }
            "latin1" => {
                assert!(
                    std::str::from_utf8(&c.bytes).is_err(),
                    "latin1 with ñ is NOT valid UTF-8"
                );
                // Manual Latin-1 decoding: byte → codepoint.
                let decoded: String = c.bytes.iter().map(|&b| char::from(b)).collect();
                assert_eq!(decoded, c.decoded);
            }
            "shift_jis" => {
                assert!(
                    std::str::from_utf8(&c.bytes).is_err(),
                    "multibyte Shift-JIS is NOT valid UTF-8"
                );
                assert_eq!(c.decoded, "テスト\n");
            }
            "utf8_bom" => {
                assert_eq!(&c.bytes[..3], &[0xEF, 0xBB, 0xBF], "UTF-8 BOM");
                assert_eq!(std::str::from_utf8(&c.bytes[3..]).unwrap(), c.decoded);
            }
            "utf8_plain" => {
                assert_eq!(std::str::from_utf8(&c.bytes).unwrap(), c.decoded);
                assert!(!c.bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "NO BOM");
            }
            "gb18030" => {
                assert_eq!(&c.bytes[..4], b"\x95\x32\x82\x36", "4 GB18030 bytes");
                assert!(c.decoded.starts_with('\u{20000}'), "exclusive zone");
            }
            "cjk_utf8_lead_f1" => {
                // Valid UTF-8 with 0xF1 as the LEAD byte of a 4-byte char.
                assert_eq!(std::str::from_utf8(&c.bytes).unwrap(), c.decoded);
                assert!(c.bytes.contains(&0xF1), "lead 0xF1 present");
                assert!(
                    c.decoded.contains('\u{44001}'),
                    "the 4-byte char (F1 84 80 81) is there"
                );
            }
            "preview_bidi_ctrl_injection" => {
                // Valid UTF-8 (decodes exactly) but RIDDLED with terminal
                // hazards: RLO, an unclosed isolate, ESC+OSC and a raw C0.
                assert_eq!(std::str::from_utf8(&c.bytes).unwrap(), c.decoded);
                assert!(!c.bytes.contains(&0x00), "no NUL: detectable as text");
                assert!(
                    c.decoded.chars().any(char::is_control) && c.decoded.contains('\u{202E}'),
                    "carries raw controls and bidi (the producer MUST sanitize)"
                );
            }
            "koi8_r" => {
                assert!(std::str::from_utf8(&c.bytes).is_err());
                assert!(c.decoded.starts_with("Привет"));
                // Cyrillic only: KOI8-R and KOI8-U agree there (the corpus
                // must not depend on the variant chardetng guesses).
                assert!(c.bytes.iter().all(|&b| b != 0xA4 && b != 0xB4));
            }
            other => panic!("unexpected fixture: {other}"),
        }
    }
}
