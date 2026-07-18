//! Tests de los wrappers de aislamiento para la aguja de fs.search
//! (M4 live search): `encode_lossless` (contrato honesto sobre el gotcha de
//! `encoding_rs::encode`) y `needle_cycle` (la decisión de excluir UTF-16
//! vive JUNTO a los datos, no en el consumidor por nombre).

use norte_encoding::{Encoding, UTF_8, encode_lossless, needle_cycle, reload_cycle};

fn enc(label: &str) -> &'static Encoding {
    Encoding::for_label(label.as_bytes()).expect("encoding conocido")
}

#[test]
fn encode_lossless_mapea_o_devuelve_none() {
    // UTF-8: "año" tal cual.
    assert_eq!(encode_lossless(UTF_8, "año").unwrap(), "año".as_bytes());
    // windows-1252: ñ = 0xF1.
    assert_eq!(
        encode_lossless(enc("windows-1252"), "año").unwrap(),
        b"a\xF1o"
    );
    // π NO es mapeable en windows-1252: None (NUNCA el "&#960;" lossy que
    // `encoding_rs::encode` emitiría a ciegas).
    assert!(encode_lossless(enc("windows-1252"), "π").is_none());
    // KOI8-R no tiene ñ latina → None.
    assert!(encode_lossless(enc("koi8-r"), "ñ").is_none());
}

#[test]
fn encode_lossless_a_utf16_da_utf8_gotcha_whatwg() {
    // Gotcha WHATWG «output encoding»: UTF-16LE/BE son SOLO de decodificación;
    // `encoding_rs::encode` los sustituye por UTF-8. 'A' → [0x41] (UTF-8), NO
    // [0x41, 0x00] (UTF-16). Por eso needle_cycle excluye UTF-16 (duplicaría
    // la aguja UTF-8) y un fichero genuinamente UTF-16 se busca decodificando.
    assert_eq!(encode_lossless(enc("utf-16le"), "A").unwrap(), b"A");
    assert_eq!(
        encode_lossless(enc("utf-16be"), "año").unwrap(),
        "año".as_bytes()
    );
}

#[test]
fn needle_cycle_es_reload_cycle_menos_utf16() {
    let needle = needle_cycle();
    let reload = reload_cycle();
    assert_eq!(needle.len(), reload.len() - 2);
    // Ni UTF-16LE ni UTF-16BE están en el set de agujas.
    assert!(!needle.iter().any(|e| e.name().starts_with("UTF-16")));
    // Pero UTF-8 y windows-1252 sí (los legacy latinos son el caso real).
    assert!(needle.iter().any(|e| e.name() == "UTF-8"));
    assert!(needle.iter().any(|e| e.name() == "windows-1252"));
}
