//! Fase 10c — fuzz del decoder de framing NDJSON (ADR 0011): streams
//! adversarios/aleatorios NO deben hacer panic ni OOM, cada frame sale limpio,
//! y el troceado del stream en `push()` no cambia los frames producidos.

use norte_proto::wire::{FrameDecoder, FrameOversized, MAX_FRAME_BYTES};
use proptest::prelude::*;

/// Drena todos los frames disponibles.
fn drain(d: &mut FrameDecoder) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(f) = d.next_frame() {
        out.push(f);
    }
    out
}

proptest! {
    /// El troceado del stream en `push()` es IRRELEVANTE: los mismos bytes,
    /// alimentados enteros o partidos en puntos arbitrarios, producen la MISMA
    /// secuencia de frames. (Tamaño < MAX_FRAME_BYTES para no cruzar el camino
    /// de oversize, que sí depende del troceado por diseño.)
    #[test]
    fn chunk_boundary_independence(
        data in proptest::collection::vec(any::<u8>(), 0..4096),
        raw_splits in proptest::collection::vec(any::<usize>(), 0..16),
    ) {
        // Entero.
        let mut d1 = FrameDecoder::new();
        d1.push(&data).expect("bajo MAX nunca es oversized");
        let whole = drain(&mut d1);

        // Partido en puntos arbitrarios (ordenados y acotados al rango).
        let mut points: Vec<usize> =
            raw_splits.iter().map(|s| s % (data.len() + 1)).collect();
        points.sort_unstable();
        let mut d2 = FrameDecoder::new();
        let mut idx = 0;
        for &p in &points {
            d2.push(&data[idx..p]).expect("bajo MAX");
            idx = p;
        }
        d2.push(&data[idx..]).expect("bajo MAX");
        let split = drain(&mut d2);

        prop_assert_eq!(&whole, &split, "el troceado cambió los frames");

        // Ningún frame emitido lleva `\n` ni es vacío (las líneas vacías se
        // descartan como keepalive).
        for f in &whole {
            prop_assert!(!f.contains(&b'\n'), "un frame no puede contener `\\n`");
            prop_assert!(!f.is_empty(), "un frame vacío no se emite");
        }
    }

    /// El decoder RESINCRONIZA en el `\n`: tras cualquier basura previa, un
    /// `\n` cierra el residuo parcial y el siguiente frame bien formado sale
    /// limpio. (El `\n` inicial es imprescindible: sin él, un residuo sin
    /// cerrar prefijaría al frame — comportamiento correcto de concatenación.)
    #[test]
    fn resyncs_on_newline_after_arbitrary_input(
        first in proptest::collection::vec(any::<u8>(), 0..2048),
    ) {
        let mut d = FrameDecoder::new();
        d.push(&first).expect("bajo MAX");
        let _ = drain(&mut d);
        // `\n` cierra el residuo, luego un frame limpio.
        d.push(b"\ndespues\n").expect("bajo MAX");
        let rest = drain(&mut d);
        prop_assert!(
            rest.iter().any(|f| f == b"despues"),
            "tras un `\\n` el decoder emite el frame limpio: {rest:?}"
        );
    }
}

#[test]
fn oversize_unterminated_frame_errors() {
    // 16 MiB + 1 SIN `\n` → FrameOversized (no acumula sin fin → sin OOM).
    let mut d = FrameDecoder::new();
    let big = vec![b'x'; MAX_FRAME_BYTES + 1];
    assert!(matches!(d.push(&big), Err(FrameOversized)));
}

#[test]
fn terminated_frame_over_max_is_accepted_by_design() {
    // Un frame que supera MAX pero LLEVA `\n` no es oversized (el `\n` lo
    // acota): documenta la semántica de ADR 0011 ("oversized = sin `\n`").
    let mut d = FrameDecoder::new();
    let mut big = vec![b'y'; MAX_FRAME_BYTES + 10];
    big.push(b'\n');
    assert!(d.push(&big).is_ok());
    assert!(d.next_frame().is_some());
}
