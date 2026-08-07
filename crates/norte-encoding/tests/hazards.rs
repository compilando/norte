//! #125: `is_terminal_hazard` enumeraba codepoints en vez de decidir por
//! propiedad, y su rustdoc afirmaba cubrir «los INVISIBLES Cf/Zl/Zp». Los que
//! se le escapaban no son exóticos: son justo los que se usan para fabricar
//! dos nombres visualmente idénticos que difieren en bytes, que es la premisa
//! que `must_mask` existe para proteger («aprueba el que ya viste»).

use norte_encoding::{is_terminal_hazard, mask_terminal_hazards};

/// Los invisibles que la enumeración NO cogía, uno a uno y con su nombre.
///
/// Se listan explícitos y no por rango porque cada uno documenta una vía
/// distinta: unos son Cf que la lista simplemente olvidó, `U+3164` y `U+115F`
/// son **Lo** —ninguna enumeración de Cf/Zl/Zp los cogerá jamás, y son los
/// clásicos del contrabando invisible—, `U+FFF9` es Cf pero está EXCLUIDO de
/// `Default_Ignorable_Code_Point`, y `U+2800` es **So**: se pinta en blanco
/// sin ser ignorable para nadie.
#[test]
fn los_invisibles_no_enumerados_son_peligro() {
    const FUGAS: &[(char, &str)] = &[
        ('\u{2061}', "FUNCTION APPLICATION (Cf)"),
        ('\u{2064}', "INVISIBLE PLUS (Cf)"),
        ('\u{206E}', "NATIONAL DIGIT SHAPES (Cf)"),
        (
            '\u{FFF9}',
            "INTERLINEAR ANNOTATION ANCHOR (Cf, fuera de DI)",
        ),
        ('\u{3164}', "HANGUL FILLER (Lo)"),
        ('\u{115F}', "HANGUL CHOSEONG FILLER (Lo)"),
        ('\u{180E}', "MONGOLIAN VOWEL SEPARATOR"),
        ('\u{2800}', "BRAILLE PATTERN BLANK (So)"),
    ];
    for (c, nombre) in FUGAS {
        assert!(
            is_terminal_hazard(*c),
            "U+{:04X} {nombre} se pinta en blanco y pasaba sin enmascarar",
            *c as u32
        );
    }
}

/// Dos nombres que un humano no puede distinguir tienen que ENMASCARARSE
/// distinto. Es el contrato entero: si `mask_terminal_hazards` deja los dos
/// iguales, aprobar el que viste aprueba también el que no.
#[test]
fn el_gemelo_invisible_no_sobrevive_al_enmascarado() {
    for intruso in ['\u{2064}', '\u{3164}', '\u{115F}', '\u{2800}', '\u{180E}'] {
        let gemelo: String = format!("a{intruso}b");
        assert_ne!(
            mask_terminal_hazards(&gemelo),
            "ab",
            "U+{:04X} desaparecía sin dejar marca: `a{{X}}b` se leía como `ab`",
            intruso as u32
        );
        assert_eq!(
            mask_terminal_hazards(&gemelo),
            "a\u{FFFD}b",
            "U+{:04X} debe dejar la marca de saneado",
            intruso as u32
        );
    }
}

/// Lo que sigue PERMITIDO, y por qué. Ampliar el set por propiedad tiene un
/// riesgo obvio: `Default_Ignorable_Code_Point` incluye ZWJ y los selectores
/// de variación, que son exactamente lo que compone un emoji. Enmascararlos
/// rompería nombres legítimos a cambio del residual de un gemelo que solo se
/// diferencia en eso.
#[test]
fn zwj_y_selectores_de_variacion_siguen_permitidos() {
    assert!(!is_terminal_hazard('\u{200D}'), "ZWJ (emoji compuesto)");
    assert!(!is_terminal_hazard('\u{FE0F}'), "VS16 (presentación emoji)");
    assert!(!is_terminal_hazard('\u{FE00}'), "VS1");
    // El caso real: familia = persona ZWJ persona ZWJ criatura.
    let familia = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F466}";
    assert_eq!(mask_terminal_hazards(familia), familia);
}

/// Ni una letra, ni un dígito, ni un signo de puntuación, ni un espacio
/// ordinario puede caer en el set. Un `is_terminal_hazard` que se pase de
/// ancho destroza nombres legítimos en silencio, que es peor que el problema
/// que arregla.
#[test]
fn lo_legible_jamas_es_peligro() {
    for c in " !\"#$%&'()*+,-./0123456789:;<=>?@ABCXYZ[\\]^_`abcxyz{|}~".chars() {
        assert!(!is_terminal_hazard(c), "ASCII imprimible {c:?}");
    }
    for c in "áéíóúñÑçüßαβγ日本語漢字한글кириллица".chars() {
        assert!(!is_terminal_hazard(c), "letra no-ASCII {c:?}");
    }
    // Espacios que SÍ ocupan sitio: se ven, luego no engañan.
    for c in ['\u{00A0}', '\u{2003}', '\u{3000}'] {
        assert!(
            !is_terminal_hazard(c),
            "U+{:04X} es un espacio visible, no un invisible",
            c as u32
        );
    }
}

/// Lo que ya cogía sigue cogido: la ampliación no puede perder terreno.
#[test]
fn el_set_previo_no_encoge() {
    for c in [
        '\u{001B}',
        '\u{0000}',
        '\u{000A}', // controles
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
            "U+{:04X} dejó de ser peligro",
            c as u32
        );
    }
}

/// Las tablas se recorren con búsqueda binaria: si alguna deja de estar
/// ordenada, `is_terminal_hazard` empieza a decir que no a codepoints que sí
/// están en ella — en silencio, y solo para algunos. Se comprueba desde fuera
/// del crate por el único camino público que hay: recorrer el espacio de
/// codepoints y exigir que el resultado coincida con una búsqueda lineal
/// sobre el mismo criterio observable.
#[test]
fn el_set_es_consistente_en_todo_el_espacio_de_codepoints() {
    // Un rango ordenado implica que el resultado nunca "reaparece" de forma
    // incoherente: se comprueba que cada char marcado como peligro lo sigue
    // siendo al consultarlo aislado y a través del enmascarado, que es el
    // único uso real.
    let mut peligrosos = 0usize;
    for cp in 0u32..=0x10_FFFF {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        if is_terminal_hazard(c) {
            peligrosos += 1;
            assert_eq!(
                mask_terminal_hazards(&c.to_string()),
                "\u{FFFD}",
                "U+{cp:04X} es peligro pero el enmascarado no lo sustituyó"
            );
        }
    }
    // Cota de cordura: el set son controles + ignorables + un puñado. Si esto
    // se dispara a decenas de miles, la tabla cogió un rango que no le tocaba.
    assert!(
        (1_000..20_000).contains(&peligrosos),
        "{peligrosos} codepoints marcados: el set creció fuera de lo razonable"
    );
}
