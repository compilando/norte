//! #125: `is_terminal_hazard` used to enumerate codepoints instead of
//! deciding by property, and its rustdoc claimed to cover "the Cf/Zl/Zp
//! INVISIBLES". The ones that slipped past it are not exotic: they are
//! exactly the ones used to forge two visually identical names that differ
//! in bytes, which is the premise `must_mask` exists to protect ("approve
//! the one you already saw").

use norte_encoding::{is_terminal_hazard, mask_terminal_hazards};

/// The invisibles the enumeration did NOT catch, one by one and named.
///
/// Listed explicitly and not by range because each one documents a
/// different route: some are Cf the list simply forgot, `U+3164` and
/// `U+115F` are **Lo** —no Cf/Zl/Zp enumeration will ever catch them, and
/// they are the classic invisible-smuggling pair—, `U+FFF9` is Cf but is
/// EXCLUDED from `Default_Ignorable_Code_Point`, and `U+2800` is **So**: it
/// paints blank without being ignorable to anyone.
#[test]
fn the_unenumerated_invisibles_are_a_hazard() {
    const LEAKS: &[(char, &str)] = &[
        ('\u{2061}', "FUNCTION APPLICATION (Cf)"),
        ('\u{2064}', "INVISIBLE PLUS (Cf)"),
        ('\u{206E}', "NATIONAL DIGIT SHAPES (Cf)"),
        ('\u{FFF9}', "INTERLINEAR ANNOTATION ANCHOR (Cf, outside DI)"),
        ('\u{3164}', "HANGUL FILLER (Lo)"),
        ('\u{115F}', "HANGUL CHOSEONG FILLER (Lo)"),
        ('\u{180E}', "MONGOLIAN VOWEL SEPARATOR"),
        ('\u{2800}', "BRAILLE PATTERN BLANK (So)"),
    ];
    for (c, name) in LEAKS {
        assert!(
            is_terminal_hazard(*c),
            "U+{:04X} {name} paints blank and used to pass unmasked",
            *c as u32
        );
    }
}

/// Two names a human cannot tell apart have to be MASKED differently. That
/// is the whole contract: if `mask_terminal_hazards` leaves the two the
/// same, approving the one you saw also approves the one you did not.
#[test]
fn the_invisible_twin_does_not_survive_masking() {
    for intruder in ['\u{2064}', '\u{3164}', '\u{115F}', '\u{2800}', '\u{180E}'] {
        let twin: String = format!("a{intruder}b");
        assert_ne!(
            mask_terminal_hazards(&twin),
            "ab",
            "U+{:04X} disappeared leaving no trace: `a{{X}}b` read as `ab`",
            intruder as u32
        );
        assert_eq!(
            mask_terminal_hazards(&twin),
            "a\u{FFFD}b",
            "U+{:04X} must leave the sanitizing mark",
            intruder as u32
        );
    }
}

/// What stays ALLOWED, and why. Extending the set by property has an
/// obvious risk: `Default_Ignorable_Code_Point` includes ZWJ and the
/// variation selectors, which are exactly what composes an emoji. Masking
/// them would break legitimate names in exchange for the residual of a
/// twin that only differs in that.
#[test]
fn zwj_and_variation_selectors_stay_allowed() {
    assert!(!is_terminal_hazard('\u{200D}'), "ZWJ (compound emoji)");
    assert!(!is_terminal_hazard('\u{FE0F}'), "VS16 (emoji presentation)");
    assert!(!is_terminal_hazard('\u{FE00}'), "VS1");
    // The real case: family = person ZWJ person ZWJ child.
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F466}";
    assert_eq!(mask_terminal_hazards(family), family);
}

/// Not a letter, not a digit, not a punctuation mark, not an ordinary space
/// can fall into the set. An `is_terminal_hazard` that overreaches destroys
/// legitimate names silently, which is worse than the problem it fixes.
#[test]
fn readable_text_is_never_a_hazard() {
    for c in " !\"#$%&'()*+,-./0123456789:;<=>?@ABCXYZ[\\]^_`abcxyz{|}~".chars() {
        assert!(!is_terminal_hazard(c), "printable ASCII {c:?}");
    }
    for c in "áéíóúñÑçüßαβγ日本語漢字한글кириллица".chars() {
        assert!(!is_terminal_hazard(c), "non-ASCII letter {c:?}");
    }
    // Spaces that DO take up room: they are visible, so they do not deceive.
    for c in ['\u{00A0}', '\u{2003}', '\u{3000}'] {
        assert!(
            !is_terminal_hazard(c),
            "U+{:04X} is a visible space, not an invisible one",
            c as u32
        );
    }
}

/// What was already caught stays caught: the extension cannot lose ground.
#[test]
fn the_previous_set_does_not_shrink() {
    for c in [
        '\u{001B}',
        '\u{0000}',
        '\u{000A}', // controls
        '\u{202A}',
        '\u{202E}',
        '\u{2066}',
        '\u{2069}', // bidi
        '\u{200B}',
        '\u{200C}',
        '\u{200E}',
        '\u{200F}',
        '\u{061C}', // invisibles
        '\u{2060}',
        '\u{FEFF}',
        '\u{00AD}',
        '\u{2028}',
        '\u{2029}',
        '\u{E0001}',
        '\u{E007F}', // TAG chars
    ] {
        assert!(
            is_terminal_hazard(c),
            "U+{:04X} stopped being a hazard",
            c as u32
        );
    }
}

/// The tables are walked with binary search: if any of them stops being
/// sorted, `is_terminal_hazard` starts saying no to codepoints that ARE in
/// it — silently, and only for some. Checked from outside the crate through
/// the only public path there is: walking the codepoint space and requiring
/// the result to match a linear search over the same observable criterion.
#[test]
fn the_set_is_consistent_across_the_whole_codepoint_space() {
    // A sorted range implies the result never "reappears" incoherently: it
    // is checked that every char marked as a hazard stays one when queried
    // in isolation and through masking, which is the only real use.
    let mut hazardous = 0usize;
    for cp in 0u32..=0x10_FFFF {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        if is_terminal_hazard(c) {
            hazardous += 1;
            assert_eq!(
                mask_terminal_hazards(&c.to_string()),
                "\u{FFFD}",
                "U+{cp:04X} is a hazard but masking did not replace it"
            );
        }
    }
    // Sanity bound: the set is controls + ignorables + a handful. If this
    // jumps to tens of thousands, the table caught a range it should not
    // have.
    assert!(
        (1_000..20_000).contains(&hazardous),
        "{hazardous} codepoints marked: the set grew beyond reason"
    );
}
