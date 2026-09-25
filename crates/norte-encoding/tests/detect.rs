//! Detection and decoding tests (spec §6): BOM → binary heuristic (NUL) →
//! chardetng. The testkit's CONTENT corpus is the yardstick: every fixture
//! must detect as text and decode EXACTLY.

use norte_encoding::{Decoded, Detection, Eol, decode, detect, detect_eol, reload_cycle};

#[test]
fn the_content_corpus_detects_and_decodes_exactly() {
    for f in norte_testkit::corpus::content_fixtures() {
        let det = detect(&f.bytes);
        let Detection::Text { encoding, .. } = det else {
            panic!("{}: detected as binary", f.id);
        };
        let Decoded {
            text, had_errors, ..
        } = decode(&f.bytes, encoding, true);
        assert!(!had_errors, "{}: detection picked a broken encoding", f.id);
        assert_eq!(text, f.decoded, "{}: bytes → EXACT text", f.id);
    }
}

/// #101: the LOSSY corpus detects as text but its canonical decode marks
/// `had_errors` (and produces exactly the `decoded` with `U+FFFD`). The flip
/// side of [`the_content_corpus_detects_and_decodes_exactly`]: that corpus is
/// lossless by contract; this one is the "detected text, broken bytes"
/// needle that feeds the plugin preview's `lossy` signal.
#[test]
fn the_lossy_corpus_detects_as_text_but_marks_had_errors() {
    for f in norte_testkit::corpus::lossy_content_fixtures() {
        let Detection::Text { encoding, .. } = detect(&f.bytes) else {
            panic!("{}: must detect as text", f.id);
        };
        let Decoded {
            text, had_errors, ..
        } = decode(&f.bytes, encoding, true);
        assert!(had_errors, "{}: a lossy decode must mark had_errors", f.id);
        assert_eq!(
            text, f.decoded,
            "{}: the expected `U+FFFD` in its place",
            f.id
        );
    }
}

#[test]
fn decoding_with_the_corpus_label_is_exact() {
    for f in norte_testkit::corpus::content_fixtures() {
        let enc = norte_encoding::Encoding::for_label(f.encoding.as_bytes())
            .unwrap_or_else(|| panic!("{}: unknown label {}", f.id, f.encoding));
        let d = decode(&f.bytes, enc, true);
        assert!(!d.had_errors, "{}", f.id);
        assert_eq!(d.text, f.decoded, "{}", f.id);
    }
}

#[test]
fn bom_rules() {
    let mut utf8_bom = vec![0xEF, 0xBB, 0xBF];
    utf8_bom.extend_from_slice("hello".as_bytes());
    match detect(&utf8_bom) {
        Detection::Text { encoding, bom } => {
            assert_eq!(encoding.name(), "UTF-8");
            assert!(bom);
        }
        Detection::Binary => panic!("UTF-8 BOM is text"),
    }
    // The BOM does not appear in the decoded text.
    let d = decode(&utf8_bom, norte_encoding::UTF_8, true);
    assert_eq!(d.text, "hello");
}

#[test]
fn nul_with_no_bom_is_binary() {
    let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
    assert!(matches!(detect(png), Detection::Binary));
    assert!(matches!(detect(b"abc\x00def"), Detection::Binary));
    // Clean UTF-8 is never binary.
    assert!(matches!(
        detect("normal text\n".as_bytes()),
        Detection::Text { .. }
    ));
}

#[test]
fn detected_eol() {
    assert_eq!(detect_eol("a\nb\n"), Eol::Lf);
    assert_eq!(detect_eol("a\r\nb\r\n"), Eol::CrLf);
    assert_eq!(detect_eol("a\rb\r"), Eol::Cr);
    assert_eq!(detect_eol("a\r\nb\n"), Eol::Mixed);
    assert_eq!(detect_eol("no line breaks"), Eol::None);
}

#[test]
fn the_reload_cycle_covers_the_corpus() {
    let cycle = reload_cycle();
    assert!(cycle.len() >= 5);
    for f in norte_testkit::corpus::content_fixtures() {
        let enc = norte_encoding::Encoding::for_label(f.encoding.as_bytes()).unwrap();
        assert!(
            cycle.iter().any(|c| std::ptr::eq(*c, enc)),
            "{} ({}) must be in the \"reload as\" cycle",
            f.id,
            f.encoding
        );
    }
}

/// Audit finding H1: forcing an encoding must BEAT the BOM (spec §6.2:
/// "always correctable by hand") — FE FF can be windows-1252 data.
#[test]
fn a_forced_encoding_beats_the_bom() {
    for f in norte_testkit::corpus::content_fixtures_forced() {
        let enc = norte_encoding::Encoding::for_label(f.encoding.as_bytes()).unwrap();
        let d = norte_encoding::decode_forced(&f.bytes, enc, true);
        assert!(!d.had_errors, "{}: forced with no losses", f.id);
        assert_eq!(d.text, f.decoded, "{}: forced EXACT", f.id);
    }
    // And UTF-16 with no BOM falls to binary in detection (contract:
    // recoverable only by hand — the hexview + "reload as…").
    for f in norte_testkit::corpus::content_fixtures_forced() {
        if f.id.contains("nobom") {
            assert!(
                matches!(detect(&f.bytes), Detection::Binary),
                "{}: with no BOM the NUL heuristic rules",
                f.id
            );
        }
    }
}

/// H3: a VALID file truncated mid-sequence must not mark "losses" —
/// `complete: false` leaves the tail pending with no error.
#[test]
fn truncated_is_not_a_loss() {
    let cut = b"a\xC3"; // ñ split
    let whole = decode(cut, norte_encoding::UTF_8, true);
    assert!(whole.had_errors, "complete: the cut IS a loss");
    let partial = decode(cut, norte_encoding::UTF_8, false);
    assert!(!partial.had_errors, "truncated: the tail is left pending");
    assert_eq!(partial.text, "a");
}

/// Focus 5: forced UTF-16 with odd length — REAL, marked loss.
#[test]
fn odd_utf16_marks_a_loss() {
    let d = norte_encoding::decode_forced(
        &[0xFF, 0xFE, 0x68, 0x00, 0x6F],
        norte_encoding::Encoding::for_label(b"utf-16le").unwrap(),
        true,
    );
    assert!(d.had_errors);
    assert!(d.text.contains('\u{FFFD}'));
}
