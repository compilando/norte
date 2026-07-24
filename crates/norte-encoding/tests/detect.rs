//! Tests de la detección y decodificación (spec §6): BOM → heurística de
//! binario (NUL) → chardetng. El corpus de CONTENIDOS del testkit es la
//! vara: cada fixture debe detectarse como texto y decodificar EXACTO.

use norte_encoding::{Decoded, Detection, Eol, decode, detect, detect_eol, reload_cycle};

#[test]
fn el_corpus_de_contenidos_se_detecta_y_decodifica_exacto() {
    for f in norte_testkit::corpus::content_fixtures() {
        let det = detect(&f.bytes);
        let Detection::Text { encoding, .. } = det else {
            panic!("{}: detectado como binario", f.id);
        };
        let Decoded {
            text, had_errors, ..
        } = decode(&f.bytes, encoding, true);
        assert!(
            !had_errors,
            "{}: la detección eligió un encoding roto",
            f.id
        );
        assert_eq!(text, f.decoded, "{}: bytes → texto EXACTO", f.id);
    }
}

/// #101: el corpus LOSSY se detecta como texto pero su decode canónico marca
/// `had_errors` (y produce exactamente el `decoded` con `U+FFFD`). La cara B de
/// [`el_corpus_de_contenidos_se_detecta_y_decodifica_exacto`]: aquel corpus es
/// sin pérdida por contrato; este es la aguja de «detectado texto, bytes rotos»
/// que alimenta la señal `lossy` de la preview de plugin.
#[test]
fn el_corpus_lossy_se_detecta_como_texto_pero_marca_had_errors() {
    for f in norte_testkit::corpus::lossy_content_fixtures() {
        let Detection::Text { encoding, .. } = detect(&f.bytes) else {
            panic!("{}: debe detectarse como texto", f.id);
        };
        let Decoded {
            text, had_errors, ..
        } = decode(&f.bytes, encoding, true);
        assert!(
            had_errors,
            "{}: decode con pérdida debe marcar had_errors",
            f.id
        );
        assert_eq!(
            text, f.decoded,
            "{}: el `U+FFFD` esperado en su sitio",
            f.id
        );
    }
}

#[test]
fn decodificar_con_la_etiqueta_del_corpus_es_exacto() {
    for f in norte_testkit::corpus::content_fixtures() {
        let enc = norte_encoding::Encoding::for_label(f.encoding.as_bytes())
            .unwrap_or_else(|| panic!("{}: etiqueta desconocida {}", f.id, f.encoding));
        let d = decode(&f.bytes, enc, true);
        assert!(!d.had_errors, "{}", f.id);
        assert_eq!(d.text, f.decoded, "{}", f.id);
    }
}

#[test]
fn bom_manda() {
    let mut utf8_bom = vec![0xEF, 0xBB, 0xBF];
    utf8_bom.extend_from_slice("hola".as_bytes());
    match detect(&utf8_bom) {
        Detection::Text { encoding, bom } => {
            assert_eq!(encoding.name(), "UTF-8");
            assert!(bom);
        }
        Detection::Binary => panic!("BOM UTF-8 es texto"),
    }
    // El BOM no aparece en el texto decodificado.
    let d = decode(&utf8_bom, norte_encoding::UTF_8, true);
    assert_eq!(d.text, "hola");
}

#[test]
fn nul_sin_bom_es_binario() {
    let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
    assert!(matches!(detect(png), Detection::Binary));
    assert!(matches!(detect(b"abc\x00def"), Detection::Binary));
    // UTF-8 limpio jamás es binario.
    assert!(matches!(
        detect("texto normal\n".as_bytes()),
        Detection::Text { .. }
    ));
}

#[test]
fn eol_detectado() {
    assert_eq!(detect_eol("a\nb\n"), Eol::Lf);
    assert_eq!(detect_eol("a\r\nb\r\n"), Eol::CrLf);
    assert_eq!(detect_eol("a\rb\r"), Eol::Cr);
    assert_eq!(detect_eol("a\r\nb\n"), Eol::Mixed);
    assert_eq!(detect_eol("sin saltos"), Eol::None);
}

#[test]
fn el_ciclo_de_recarga_cubre_el_corpus() {
    let cycle = reload_cycle();
    assert!(cycle.len() >= 5);
    for f in norte_testkit::corpus::content_fixtures() {
        let enc = norte_encoding::Encoding::for_label(f.encoding.as_bytes()).unwrap();
        assert!(
            cycle.iter().any(|c| std::ptr::eq(*c, enc)),
            "{} ({}) debe estar en el ciclo de «recargar como»",
            f.id,
            f.encoding
        );
    }
}

/// H1 de la auditoría: forzar un encoding debe VENCER al BOM (spec §6.2:
/// «siempre corregible a mano») — FE FF puede ser dato windows-1252.
#[test]
fn el_encoding_forzado_vence_al_bom() {
    for f in norte_testkit::corpus::content_fixtures_forced() {
        let enc = norte_encoding::Encoding::for_label(f.encoding.as_bytes()).unwrap();
        let d = norte_encoding::decode_forced(&f.bytes, enc, true);
        assert!(!d.had_errors, "{}: forzado sin pérdidas", f.id);
        assert_eq!(d.text, f.decoded, "{}: forzado EXACTO", f.id);
    }
    // Y los UTF-16 sin BOM caen a binario en la detección (contrato:
    // recuperables solo a mano — el hexview + «recargar como…»).
    for f in norte_testkit::corpus::content_fixtures_forced() {
        if f.id.contains("nobom") {
            assert!(
                matches!(detect(&f.bytes), Detection::Binary),
                "{}: sin BOM la heurística NUL manda",
                f.id
            );
        }
    }
}

/// H3: un archivo VÁLIDO truncado a mitad de secuencia no puede marcar
/// «pérdidas» — `complete: false` deja la cola pendiente sin error.
#[test]
fn truncado_no_es_perdida() {
    let cortado = b"a\xC3"; // ñ partida
    let entero = decode(cortado, norte_encoding::UTF_8, true);
    assert!(entero.had_errors, "completo: el corte ES pérdida");
    let parcial = decode(cortado, norte_encoding::UTF_8, false);
    assert!(!parcial.had_errors, "truncado: la cola queda pendiente");
    assert_eq!(parcial.text, "a");
}

/// Foco 5: UTF-16 forzado con longitud impar — pérdida REAL y marcada.
#[test]
fn utf16_impar_marca_perdida() {
    let d = norte_encoding::decode_forced(
        &[0xFF, 0xFE, 0x68, 0x00, 0x6F],
        norte_encoding::Encoding::for_label(b"utf-16le").unwrap(),
        true,
    );
    assert!(d.had_errors);
    assert!(d.text.contains('\u{FFFD}'));
}
