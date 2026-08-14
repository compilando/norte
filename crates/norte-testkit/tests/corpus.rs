//! Sanidad del corpus canónico: al menos 62 fixtures (48 nombres + 11+3
//! contenidos), nombres válidos como segmentos `VPath`, contenidos con la
//! forma declarada.

use norte_testkit::corpus::{content_fixtures, hostile_chords, hostile_names, spelling_twins};

#[test]
fn corpus_counts() {
    // Suelo, no cuenta exacta (#169): antes esto y el doctest de
    // `hostile_names` aserraban `== 48` cada uno, y se ponían rojos en
    // momentos distintos porque `nextest` no corre doctests — añadir una
    // fixture dejaba el doctest rojo sin que `just t` lo viera. Un suelo
    // sigue cazando "el corpus se vació por accidente" sin que crecerlo
    // cueste tocar dos sitios.
    assert!(hostile_names().len() >= 48, "nombres hostiles");
    assert_eq!(content_fixtures().len(), 11, "contenidos detectables");
    assert_eq!(
        norte_testkit::corpus::content_fixtures_forced().len(),
        3,
        "contenidos solo-forzables"
    );
    assert_eq!(hostile_chords().len(), 4, "chords hostiles (H1)");
}

#[test]
fn hostile_chords_son_un_solo_codepoint_hazard_y_unicos() {
    let chords = hostile_chords();
    let mut seen = std::collections::HashSet::new();
    for c in &chords {
        assert!(
            norte_encoding::is_terminal_hazard(c.token),
            "[{}] debe ser un hazard de terminal",
            c.id
        );
        assert!(!c.why.is_empty(), "[{}] documenta por qué es hostil", c.id);
        assert!(seen.insert(c.token), "[{}] token duplicado", c.id);
    }
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

/// #169: las cuatro fixtures pedidas por las tres deferrals de
/// `2026-08-11-directory-sync.md`, pinando la propiedad concreta que cada
/// una dice tener y no solo que exista.
#[test]
fn fixtures_nuevas_de_169_cumplen_lo_que_prometen() {
    let names = hostile_names();
    let find = |id: &str| {
        names
            .iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("fixture {id} en el corpus"))
    };

    // `!`: el marcador de composición de archivos (ADR 0018) como nombre
    // real, byte a byte y nada más.
    assert_eq!(find("archive_marker_literal").bytes, b"!");

    // 255 bytes (NAME_MAX) que NO son UTF-8 válido, a diferencia de
    // `name_max_255` (puro ASCII).
    let tail = find("name_max_255_invalid_tail");
    assert_eq!(tail.bytes.len(), 255);
    assert!(std::str::from_utf8(&tail.bytes).is_err());

    // Un nombre que ES un sidecar `.trashinfo` completo, sin la barra que
    // `Segment` prohíbe (el `Path` real va url-encoded).
    let spoof = find("trashinfo_record_spoof");
    assert!(!spoof.bytes.contains(&b'/'));
    let texto = std::str::from_utf8(&spoof.bytes).expect("UTF-8 válido");
    assert!(texto.contains("Path="));
    assert!(texto.contains("DeletionDate="));

    // U+0130, cuyo pliegue completo son DOS codepoints ('i' + U+0307), no
    // una 'i' simple.
    let turco = find("turkish_dotted_i_capital");
    let texto = std::str::from_utf8(&turco.bytes).expect("UTF-8 válido");
    assert!(texto.starts_with('\u{0130}'));
}

/// #169: `spelling_twins()` referencia `id`s del corpus, no bytes propios —
/// los dos lados de cada par tienen que existir de verdad y ser distintos.
#[test]
fn spelling_twins_referencian_ids_reales_y_distintos() {
    let names = hostile_names();
    let ids: std::collections::HashSet<&str> = names.iter().map(|n| n.id.as_str()).collect();
    for twin in spelling_twins() {
        assert!(
            ids.contains(twin.left),
            "[{}] no está en el corpus",
            twin.left
        );
        assert!(
            ids.contains(twin.right),
            "[{}] no está en el corpus",
            twin.right
        );
        assert_ne!(
            twin.left, twin.right,
            "un par no es un nombre consigo mismo"
        );
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
            "utf8_plain" => {
                assert_eq!(std::str::from_utf8(&c.bytes).unwrap(), c.decoded);
                assert!(!c.bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "SIN BOM");
            }
            "gb18030" => {
                assert_eq!(&c.bytes[..4], b"\x95\x32\x82\x36", "4 bytes de GB18030");
                assert!(c.decoded.starts_with('\u{20000}'), "zona exclusiva");
            }
            "cjk_utf8_lead_f1" => {
                // UTF-8 válido con 0xF1 como byte LÍDER de un char de 4 bytes.
                assert_eq!(std::str::from_utf8(&c.bytes).unwrap(), c.decoded);
                assert!(c.bytes.contains(&0xF1), "0xF1 líder presente");
                assert!(
                    c.decoded.contains('\u{44001}'),
                    "el char de 4 bytes (F1 84 80 81) está"
                );
            }
            "preview_bidi_ctrl_injection" => {
                // UTF-8 válido (decodifica exacto) pero PLAGADO de hazards de
                // terminal: RLO, isolate sin cerrar, ESC+OSC y un C0 crudo.
                assert_eq!(std::str::from_utf8(&c.bytes).unwrap(), c.decoded);
                assert!(!c.bytes.contains(&0x00), "sin NUL: detectable como texto");
                assert!(
                    c.decoded.chars().any(char::is_control) && c.decoded.contains('\u{202E}'),
                    "lleva controles y bidi crudos (el productor DEBE sanear)"
                );
            }
            "koi8_r" => {
                assert!(std::str::from_utf8(&c.bytes).is_err());
                assert!(c.decoded.starts_with("Привет"));
                // Solo cirílico: KOI8-R y KOI8-U coinciden ahí (el corpus
                // no debe depender de la variante que adivine chardetng).
                assert!(c.bytes.iter().all(|&b| b != 0xA4 && b != 0xB4));
            }
            other => panic!("fixture inesperada: {other}"),
        }
    }
}
