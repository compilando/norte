//! #57: NAME reinterpretation (display-only) — cp437 totality, closed
//! cycle, and a chardetng suggestion mapped to the cycle.

use norte_encoding::{NameEncoding, decode_name, name_reinterpret_cycle, suggest_name_encoding};

/// GOLDEN for cp437's high half (audit #57 finding F4): an INDEPENDENT copy
/// of the table — a silent swap like ß→β or µ(U+00B5)→μ(U+03BC) would still
/// be "unique" and pass the structural test, but would lie to the user
/// about real names. Verified against the canonical cp437 codec.
#[test]
fn cp437_high_half_golden() {
    const GOLDEN: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»\
░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀\
αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{00A0}";
    let bytes: Vec<u8> = (0x80u8..=0xFF).collect();
    assert_eq!(decode_name(&bytes, NameEncoding::Cp437), GOLDEN);
}

/// cp437 is TOTAL and faithful: all 256 bytes decode (none falls to
/// U+FFFD) and the high half produces 128 DISTINCT chars (a table with a
/// duplicated typo would collapse two bytes onto the same glyph).
#[test]
fn cp437_is_total_and_collision_free() {
    let all: Vec<u8> = (0u8..=255).collect();
    let text = decode_name(&all, NameEncoding::Cp437);
    assert_eq!(text.chars().count(), 256);
    assert!(!text.contains('\u{FFFD}'), "cp437 maps all 256 bytes");
    let high: std::collections::BTreeSet<char> =
        decode_name(&(128u8..=255).collect::<Vec<_>>(), NameEncoding::Cp437)
            .chars()
            .collect();
    assert_eq!(high.len(), 128, "high half with no duplicate glyphs");
}

/// The cycle starts at cp437 (the zip bit11=0 historical default) and the
/// labels are stable (the UI paints them).
#[test]
fn cycle_and_labels() {
    let cycle = name_reinterpret_cycle();
    assert_eq!(cycle[0], NameEncoding::Cp437);
    let labels: Vec<&str> = cycle.iter().map(NameEncoding::label).collect();
    assert_eq!(
        labels,
        ["cp437", "IBM866", "Shift_JIS", "GBK", "windows-1252"]
    );
}

/// Cyrillic names in cp866 → chardetng suggests IBM866 (a cycle member).
/// With no samples → None.
#[test]
fn ibm866_suggestion_and_empty() {
    // "Новая папка" ("New folder") and "Документы" ("Documents") in cp866.
    let a: &[u8] = b"\x8d\xae\xa2\xa0\xef \xaf\xa0\xaf\xaa\xa0";
    let b: &[u8] = b"\x84\xae\xaa\xe3\xac\xa5\xad\xe2\xeb";
    assert_eq!(
        suggest_name_encoding(&[a, b]),
        Some(NameEncoding::Rs(encoding_rs::IBM866))
    );
    assert_eq!(suggest_name_encoding(&[]), None);
}
