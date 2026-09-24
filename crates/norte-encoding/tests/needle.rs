//! Tests for the isolation wrappers of fs.search's needle (M4 live search):
//! `encode_lossless` (an honest contract over `encoding_rs::encode`'s
//! gotcha) and `needle_cycle` (the decision to exclude UTF-16 lives
//! ALONGSIDE the data, not in the consumer, by name).

use norte_encoding::{Encoding, UTF_8, encode_lossless, needle_cycle, reload_cycle};

fn enc(label: &str) -> &'static Encoding {
    Encoding::for_label(label.as_bytes()).expect("known encoding")
}

#[test]
fn encode_lossless_maps_or_returns_none() {
    // UTF-8: "año" as-is.
    assert_eq!(encode_lossless(UTF_8, "año").unwrap(), "año".as_bytes());
    // windows-1252: ñ = 0xF1.
    assert_eq!(
        encode_lossless(enc("windows-1252"), "año").unwrap(),
        b"a\xF1o"
    );
    // π is NOT mappable in windows-1252: None (NEVER the lossy "&#960;"
    // `encoding_rs::encode` would emit blindly).
    assert!(encode_lossless(enc("windows-1252"), "π").is_none());
    // KOI8-R has no Latin ñ → None.
    assert!(encode_lossless(enc("koi8-r"), "ñ").is_none());
}

#[test]
fn encode_lossless_to_utf16_gives_the_whatwg_utf8_gotcha() {
    // WHATWG "output encoding" gotcha: UTF-16LE/BE are DECODE-ONLY;
    // `encoding_rs::encode` substitutes them with UTF-8. 'A' → [0x41]
    // (UTF-8), NOT [0x41, 0x00] (UTF-16). That is why needle_cycle excludes
    // UTF-16 (it would duplicate the UTF-8 needle) and a genuinely UTF-16
    // file is searched by decoding.
    assert_eq!(encode_lossless(enc("utf-16le"), "A").unwrap(), b"A");
    assert_eq!(
        encode_lossless(enc("utf-16be"), "año").unwrap(),
        "año".as_bytes()
    );
}

#[test]
fn needle_cycle_is_reload_cycle_minus_utf16() {
    let needle = needle_cycle();
    let reload = reload_cycle();
    assert_eq!(needle.len(), reload.len() - 2);
    // Neither UTF-16LE nor UTF-16BE are in the needle set.
    assert!(!needle.iter().any(|e| e.name().starts_with("UTF-16")));
    // But UTF-8 and windows-1252 are (legacy Latin is the real case).
    assert!(needle.iter().any(|e| e.name() == "UTF-8"));
    assert!(needle.iter().any(|e| e.name() == "windows-1252"));
}
