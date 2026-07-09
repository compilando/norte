//! Estrategias proptest canónicas (spec §12): las publican TODOS los crates
//! que hagan property-based testing sobre paths. Las copias locales de
//! `norte-proto/tests` (que no puede depender de este crate: ciclo) deben
//! mantenerse alineadas con estas.

use norte_proto::{Authority, Scheme, Segment, VPath};
use proptest::prelude::*;

use crate::corpus;

/// Bytes válidos como segmento: 1–64 bytes, sin NUL, sin `/`, sin `.`/`..`.
pub fn arb_segment_bytes() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 1..64)
        .prop_filter("sin NUL ni separador", |b| {
            !b.contains(&0x00) && !b.contains(&0x2F)
        })
        .prop_filter("sin dot-segments", |b| b != b"." && b != b"..")
}

/// Schemes válidos (`[a-z][a-z0-9+.-]*`).
///
/// # Panics
/// Nunca: la regex es constante y válida.
pub fn arb_scheme() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z][a-z0-9+.-]{0,10}").expect("regex válida")
}

/// Authorities válidas u ausentes (charset completo: ASCII imprimible sin
/// `%` ni `/`).
///
/// # Panics
/// Nunca: la regex es constante y solo genera authorities válidas.
pub fn arb_authority() -> impl Strategy<Value = Option<Authority>> {
    let valid = proptest::string::string_regex("[!-$&-.0-~]{1,16}").expect("regex válida");
    proptest::option::of(
        valid.prop_map(|s| Authority::new(&s).expect("estrategia genera authorities válidas")),
    )
}

/// `VPath`s arbitrarios: scheme + authority opcional + 0–8 segmentos de bytes.
///
/// # Panics
/// Nunca: compone estrategias que solo generan componentes válidos.
pub fn arb_vpath() -> impl Strategy<Value = VPath> {
    (
        arb_scheme(),
        arb_authority(),
        proptest::collection::vec(arb_segment_bytes(), 0..8),
    )
        .prop_map(|(scheme, authority, segs)| {
            let scheme = Scheme::new(&scheme).expect("estrategia genera schemes válidos");
            let mut p = VPath::root(scheme, authority);
            for s in segs {
                p = p.join(Segment::new(s).expect("estrategia genera segmentos válidos"));
            }
            p
        })
}

/// Nombres de archivo hostiles: 50% un caso del corpus canónico, 50% bytes
/// arbitrarios válidos como segmento. Sesgado a lo que rompe software real.
///
/// Ojo con `MemProvider` case-insensitive: su fold es ASCII, y bytes trail
/// de multibyte legacy (Shift-JIS) pueden dar `CaseCollision` espuria — no
/// asertes semántica de colisión Unicode contra el simulador.
///
/// # Panics
/// Nunca: el corpus embebido no está vacío.
pub fn arb_hostile_filename() -> impl Strategy<Value = Vec<u8>> {
    let from_corpus: Vec<Vec<u8>> = corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        .collect();
    prop_oneof![proptest::sample::select(from_corpus), arb_segment_bytes(),]
}
