//! Fuzz corto (proptest) del framing + envelope JSON-RPC (spec §12, ADR 0011).
//! Sin sockets: el `FrameDecoder` y el parse de envelope son I/O-free, así que
//! se fuzzean con bytes puros. Corre en el gate de PR (como `config_fuzz` de la
//! TUI); el nightly amplía casos.

use norte_proto::wire::{
    FrameDecoder, JsonRpcVersion, Message, MessageKind, Notification, Request, RequestId, classify,
    encode_frame,
};
use proptest::prelude::*;

/// Drena todos los frames que el decoder tenga listos.
fn drain(dec: &mut FrameDecoder) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(f) = dec.next_frame() {
        out.push(f);
    }
    out
}

proptest! {
    /// Ningún byte arbitrario hace `panic` al decoder, y todo frame emitido
    /// está LIMPIO: sin `\n` interior y sin `\r` final (el contrato de NDJSON).
    #[test]
    fn framing_never_panics_frames_are_clean(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let mut dec = FrameDecoder::new();
        // Inputs pequeños: jamás rozan MAX_FRAME_BYTES, el push no puede fallar.
        prop_assert!(dec.push(&data).is_ok());
        for frame in drain(&mut dec) {
            prop_assert!(!frame.contains(&b'\n'), "frame con \\n interior");
            prop_assert_ne!(frame.last(), Some(&b'\r'), "frame con \\r final sin recortar");
            prop_assert!(!frame.is_empty(), "línea vacía debió descartarse");
        }
    }

    /// Invariante de frontera de chunk: alimentar los MISMOS bytes troceados de
    /// cualquier forma entrega EXACTAMENTE los mismos frames que en un push
    /// único. Es la propiedad que la optimización `scanned`/`pending_newlines`
    /// podría romper.
    #[test]
    fn framing_chunk_boundary_invariance(
        data in proptest::collection::vec(any::<u8>(), 0..2048),
        cuts in proptest::collection::vec(any::<prop::sample::Index>(), 0..16),
    ) {
        // Referencia: un solo push.
        let mut whole = FrameDecoder::new();
        prop_assert!(whole.push(&data).is_ok());
        let expected = drain(&mut whole);

        // Trocea `data` en puntos arbitrarios ordenados.
        let mut points: Vec<usize> = cuts.iter().map(|c| c.index(data.len() + 1)).collect();
        points.sort_unstable();
        let mut chunked = FrameDecoder::new();
        let mut got = Vec::new();
        let mut start = 0;
        for &p in points.iter().chain(std::iter::once(&data.len())) {
            let end = p.max(start).min(data.len());
            prop_assert!(chunked.push(&data[start..end]).is_ok());
            got.extend(drain(&mut chunked));
            start = end;
        }
        prop_assert_eq!(expected, got);
    }

    /// `classify` y el parse de `Message` nunca hacen `panic` con basura: el
    /// server debe poder distinguir "JSON roto" de "envelope inválido" sin
    /// caerse (spec §12).
    #[test]
    fn envelope_parse_never_panics(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&data) {
            let _ = classify(&v); // no panic
        }
        // Parse tipado directo: Ok o Err, jamás panic.
        let _ = serde_json::from_slice::<Message>(&data);
    }

    /// Roundtrip: una request/notification válida sobrevive encode_frame →
    /// FrameDecoder → parse, con method/id intactos. `method` arbitrario
    /// (incluye Unicode y control salvo `\0`, que serde_json escapa).
    #[test]
    fn envelope_request_roundtrips(
        method in "\\PC{0,64}",
        id in any::<u64>(),
        is_request in any::<bool>(),
    ) {
        let frame = if is_request {
            encode_frame(&Request {
                jsonrpc: JsonRpcVersion,
                id: RequestId::Num(id),
                method: method.clone(),
                params: None,
            })
        } else {
            encode_frame(&Notification {
                jsonrpc: JsonRpcVersion,
                method: method.clone(),
                params: None,
            })
        }
        .expect("un tipo del protocolo siempre serializa");

        let mut dec = FrameDecoder::new();
        prop_assert!(dec.push(&frame).is_ok());
        let bytes = dec.next_frame().expect("el frame cerró con \\n");
        let msg: Message = serde_json::from_slice(&bytes).expect("reparsea");

        match (is_request, &msg) {
            (true, Message::Request(r)) => {
                prop_assert_eq!(&r.method, &method);
                prop_assert_eq!(&r.id, &RequestId::Num(id));
            }
            (false, Message::Notification(n)) => prop_assert_eq!(&n.method, &method),
            other => prop_assert!(false, "clasificación inesperada: {:?}", other),
        }
        // Coherencia con classify sobre el Value.
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let want = if is_request { MessageKind::Request } else { MessageKind::Notification };
        prop_assert_eq!(classify(&v), want);
    }
}
