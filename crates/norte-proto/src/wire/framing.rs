//! NDJSON framing (ADR 0011): one JSON message per `\n`-terminated line, with
//! an anti-DoS frame cap. No I/O — the transport feeds bytes and drains
//! frames; that is why it is fuzzable without sockets (spec §12).

/// Maximum frame size (16 MiB). A frame that exceeds it without a `\n` is
/// [`FrameOversized`]: the peer is broken or hostile — it gets closed.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// The buffer went over [`MAX_FRAME_BYTES`] without closing a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("frame over {MAX_FRAME_BYTES} bytes without terminating")]
pub struct FrameOversized;

/// Incremental NDJSON decoder: accumulates bytes from the network and
/// delivers complete frames (without the `\n`; a trailing `\r` is trimmed for
/// tolerance). Empty lines are silently dropped (cheap keepalive).
///
/// ```
/// use norte_proto::wire::FrameDecoder;
/// let mut d = FrameDecoder::new();
/// d.push(b"{\"a\":1}\n{\"b\"").unwrap();
/// assert_eq!(d.next_frame(), Some(b"{\"a\":1}".to_vec()));
/// assert_eq!(d.next_frame(), None); // the second one hasn't closed yet
/// d.push(b":2}\r\n").unwrap();
/// assert_eq!(d.next_frame(), Some(b"{\"b\":2}".to_vec()));
/// ```
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    /// From where we have not seen a `\n` yet (avoids rescanning on every
    /// push).
    scanned: usize,
    /// `\n`s received and not yet drained: the oversized check is O(1) per
    /// push (no rescanning the whole buffer — a security-reviewer finding).
    pending_newlines: usize,
}

impl FrameDecoder {
    /// Empty decoder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds received bytes.
    ///
    /// # Errors
    /// [`FrameOversized`] if the frame in progress exceeds
    /// [`MAX_FRAME_BYTES`] without closing — the caller must cut the
    /// connection.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), FrameOversized> {
        self.pending_newlines += bytes
            .iter()
            .fold(0usize, |acc, &b| acc + usize::from(b == b'\n'));
        self.buf.extend_from_slice(bytes);
        // The cap applies to the frame IN PROGRESS: if there is a `\n`
        // pending to drain, it is not oversized yet.
        if self.buf.len() > MAX_FRAME_BYTES && self.pending_newlines == 0 {
            return Err(FrameOversized);
        }
        Ok(())
    }

    /// Extracts the next complete frame, if there is one. Never blocks.
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
            frame.pop(); // the `\n`
            // Trims ALL trailing `\r`s, not just one: a peer emitting
            // `\r\r\n` (or repeated CRLF) must not leave a dangling `\r` that
            // breaks the JSON parse (a framing fuzz finding).
            while frame.last() == Some(&b'\r') {
                frame.pop();
            }
            if frame.is_empty() {
                continue; // empty line = keepalive, dropped
            }
            return Some(frame);
        }
    }
}

/// Encodes a message as an NDJSON frame (compact JSON + `\n`).
///
/// ```
/// use norte_proto::wire::encode_frame;
/// let frame = encode_frame(&serde_json::json!({"a": 1})).unwrap();
/// assert_eq!(frame, b"{\"a\":1}\n");
/// ```
///
/// # Errors
/// Whatever `serde_json` returns (a protocol type always serializes; this
/// only fails with pathological caller `Value` payloads).
pub fn encode_frame<T: serde::Serialize>(msg: &T) -> Result<Vec<u8>, serde_json::Error> {
    let mut out = serde_json::to_vec(msg)?;
    out.push(b'\n');
    Ok(out)
}
