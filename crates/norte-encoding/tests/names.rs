//! #57: reinterpretación de NOMBRES (display-only) — totalidad de cp437,
//! ciclo cerrado y sugerencia de chardetng mapeada al ciclo.

use norte_encoding::{NameEncoding, decode_name, name_reinterpret_cycle, suggest_name_encoding};

/// GOLDEN de la mitad alta de cp437 (F4 del audit #57): copia INDEPENDIENTE
/// de la tabla — un swap silencioso tipo ß→β o µ(U+00B5)→μ(U+03BC) seguiría
/// siendo «único» y pasaría el test estructural, pero mentiría al usuario
/// sobre nombres reales. Verificada contra el codec cp437 canónico.
#[test]
fn cp437_mitad_alta_golden() {
    const GOLDEN: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»\
░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀\
αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{00A0}";
    let bytes: Vec<u8> = (0x80u8..=0xFF).collect();
    assert_eq!(decode_name(&bytes, NameEncoding::Cp437), GOLDEN);
}

/// cp437 es TOTAL y fiel: los 256 bytes decodifican (ninguno cae a U+FFFD)
/// y la mitad alta produce 128 chars DISTINTOS (una tabla con un typo
/// duplicado colapsaría dos bytes en el mismo glifo).
#[test]
fn cp437_es_total_y_sin_colisiones() {
    let todos: Vec<u8> = (0u8..=255).collect();
    let texto = decode_name(&todos, NameEncoding::Cp437);
    assert_eq!(texto.chars().count(), 256);
    assert!(!texto.contains('\u{FFFD}'), "cp437 mapea los 256 bytes");
    let altos: std::collections::BTreeSet<char> =
        decode_name(&(128u8..=255).collect::<Vec<_>>(), NameEncoding::Cp437)
            .chars()
            .collect();
    assert_eq!(altos.len(), 128, "mitad alta sin glifos duplicados");
}

/// El ciclo empieza en cp437 (default histórico del zip bit11=0) y las
/// etiquetas son estables (UI las pinta).
#[test]
fn ciclo_y_etiquetas() {
    let cycle = name_reinterpret_cycle();
    assert_eq!(cycle[0], NameEncoding::Cp437);
    let labels: Vec<&str> = cycle.iter().map(NameEncoding::label).collect();
    assert_eq!(
        labels,
        ["cp437", "IBM866", "Shift_JIS", "GBK", "windows-1252"]
    );
}

/// Nombres cirílicos en cp866 → chardetng sugiere IBM866 (miembro del
/// ciclo). Sin muestras → None.
#[test]
fn sugerencia_ibm866_y_vacio() {
    // "Новая папка" y "Документы" en cp866.
    let a: &[u8] = b"\x8d\xae\xa2\xa0\xef \xaf\xa0\xaf\xaa\xa0";
    let b: &[u8] = b"\x84\xae\xaa\xe3\xac\xa5\xad\xe2\xeb";
    assert_eq!(
        suggest_name_encoding(&[a, b]),
        Some(NameEncoding::Rs(encoding_rs::IBM866))
    );
    assert_eq!(suggest_name_encoding(&[]), None);
}
