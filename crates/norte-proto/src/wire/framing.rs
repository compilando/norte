//! Framing NDJSON (ADR 0011): un mensaje JSON por línea `\n`, límite de
//! frame anti-DoS. Sin I/O — el transporte alimenta bytes y drena frames;
//! por eso es fuzzeable sin sockets (spec §12).

/// Tamaño máximo de un frame (16 MiB). Un frame que lo supere sin `\n` es
/// [`FrameOversized`]: el peer está roto o es hostil — se cierra.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// El buffer superó [`MAX_FRAME_BYTES`] sin cerrar frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("frame de más de {MAX_FRAME_BYTES} bytes sin terminar")]
pub struct FrameOversized;

/// Decoder incremental de NDJSON: acumula bytes de la red y entrega frames
/// completos (sin el `\n`; un `\r` final se recorta por tolerancia).
/// Las líneas vacías se descartan en silencio (keepalive barato).
///
/// ```
/// use norte_proto::wire::FrameDecoder;
/// let mut d = FrameDecoder::new();
/// d.push(b"{\"a\":1}\n{\"b\"").unwrap();
/// assert_eq!(d.next_frame(), Some(b"{\"a\":1}".to_vec()));
/// assert_eq!(d.next_frame(), None); // el segundo aún no cerró
/// d.push(b":2}\r\n").unwrap();
/// assert_eq!(d.next_frame(), Some(b"{\"b\":2}".to_vec()));
/// ```
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    /// Desde dónde no hemos visto `\n` (evita re-escanear en cada push).
    scanned: usize,
    /// `\n` recibidos y aún no drenados: el chequeo de oversized es O(1)
    /// por push (sin re-escanear el buffer entero — hallazgo del
    /// security-reviewer).
    pending_newlines: usize,
}

impl FrameDecoder {
    /// Decoder vacío.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Alimenta bytes recibidos.
    ///
    /// # Errors
    /// [`FrameOversized`] si el frame en curso supera [`MAX_FRAME_BYTES`]
    /// sin cerrar — el caller debe cortar la conexión.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), FrameOversized> {
        self.pending_newlines += bytes
            .iter()
            .fold(0usize, |acc, &b| acc + usize::from(b == b'\n'));
        self.buf.extend_from_slice(bytes);
        // El límite aplica al FRAME en curso: si hay un `\n` pendiente de
        // drenar, todavía no es oversized.
        if self.buf.len() > MAX_FRAME_BYTES && self.pending_newlines == 0 {
            return Err(FrameOversized);
        }
        Ok(())
    }

    /// Extrae el siguiente frame completo, si lo hay. Nunca bloquea.
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        loop {
            let nl = self.buf[self.scanned..]
                .iter()
                .position(|&b| b == b'\n')
                .map(|i| i + self.scanned);
            let Some(nl) = nl else {
                self.scanned = self.buf.len();
                return None;
            };
            let mut frame: Vec<u8> = self.buf.drain(..=nl).collect();
            self.scanned = 0;
            self.pending_newlines = self.pending_newlines.saturating_sub(1);
            frame.pop(); // el `\n`
            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            if frame.is_empty() {
                continue; // línea vacía = keepalive, se descarta
            }
            return Some(frame);
        }
    }
}

/// Codifica un mensaje como frame NDJSON (JSON compacto + `\n`).
///
/// ```
/// use norte_proto::wire::encode_frame;
/// let frame = encode_frame(&serde_json::json!({"a": 1})).unwrap();
/// assert_eq!(frame, b"{\"a\":1}\n");
/// ```
///
/// # Errors
/// Los de `serde_json` (un tipo del protocolo siempre serializa; esto solo
/// falla con payloads `Value` patológicos del caller).
pub fn encode_frame<T: serde::Serialize>(msg: &T) -> Result<Vec<u8>, serde_json::Error> {
    let mut out = serde_json::to_vec(msg)?;
    out.push(b'\n');
    Ok(out)
}
