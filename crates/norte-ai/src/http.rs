//! HTTP helpers shared by the providers (ADR 0031): HTTP status → [`AiError`]
//! mapping, and the line stream (SSE/NDJSON) built over reqwest's body. Each
//! provider contributes ONLY the parser for one line; the buffering, the
//! `\n` splitting and the termination live here.

use std::collections::VecDeque;

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;

use crate::provider::{AiError, ChatRequest, ChatRole, ChatStream};

/// Cap on ONE wire (SSE/NDJSON) line. A chat delta never comes close;
/// exceeding it = hostile or broken server → [`AiError::Protocol`] (ADR
/// 0031: hostile cases are cut off typed, never accumulated without limit).
const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Result of interpreting ONE wire line.
pub(crate) enum WireEvent {
    /// Text delta to deliver to the consumer.
    Delta(String),
    /// Uninteresting line (ping, `event:`, keep-alive, content-less chunk).
    Skip,
    /// Clean end of the response (`message_stop`, `[DONE]`, `done: true`).
    Stop,
}

/// Maps the status of a response ALREADY received: 401/403 →
/// [`AiError::Auth`], 429 → [`AiError::RateLimited`] (with `retry-after` in
/// seconds if it came), any other non-2xx → [`AiError::Http`]. A 2xx passes
/// through untouched.
pub(crate) fn check_status(resp: reqwest::Response) -> Result<reqwest::Response, AiError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    Err(match status.as_u16() {
        401 | 403 => AiError::Auth,
        429 => AiError::RateLimited {
            retry_after: resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok()),
        },
        s => AiError::Http { status: s },
    })
}

/// Transport error (connection, DNS, TLS, timeout). `reqwest::Error`'s
/// `Display` does not include headers: the api key is never leaked through
/// here (rule 10).
pub(crate) fn transport(e: &reqwest::Error) -> AiError {
    AiError::Transport(e.to_string())
}

/// The payload of an SSE `data: ...` line (without the prefix nor the
/// optional space); `None` if the line is not a data line (`event:`,
/// comment, empty).
pub(crate) fn sse_data(line: &str) -> Option<&str> {
    line.strip_prefix("data:").map(str::trim_start)
}

/// Validates [`ChatRequest`]'s contract: at least one non-system turn exists
/// and the first one is `user` (all three providers require it; it is cut
/// off here, typed, before touching the network).
pub(crate) fn validate_turns(req: &ChatRequest) -> Result<(), AiError> {
    match req.messages.iter().find(|m| m.role != ChatRole::System) {
        Some(m) if m.role == ChatRole::User => Ok(()),
        _ => Err(AiError::Protocol(
            "the conversation's first turn must be `user`".into(),
        )),
    }
}

/// State of the line stream: owns reqwest's body (dropping the returned
/// stream drops the body → reqwest aborts the HTTP request; rule 3,
/// drop-based cancellation).
struct LineState<F> {
    body: BoxStream<'static, reqwest::Result<Bytes>>,
    buf: Vec<u8>,
    ready: VecDeque<String>,
    eof: bool,
    stopped: bool,
    parse: F,
}

/// Stream of deltas over `resp`'s body: buffers bytes, splits on `\n`
/// (trimming ALL trailing `\r`s), and passes each complete line through
/// `parse`. An `Err` from the parser or the transport ENDS the stream after
/// being emitted; [`WireEvent::Stop`] ends it without emitting anything
/// else. The remaining bytes at EOF (a final line with no `\n`) also go
/// through the parser.
pub(crate) fn delta_stream<F>(resp: reqwest::Response, parse: F) -> ChatStream
where
    F: FnMut(&str) -> Result<WireEvent, AiError> + Send + 'static,
{
    let state = LineState {
        body: resp.bytes_stream().boxed(),
        buf: Vec::new(),
        ready: VecDeque::new(),
        eof: false,
        stopped: false,
        parse,
    };
    futures::stream::unfold(state, |mut st| async move {
        loop {
            if st.stopped {
                return None;
            }
            if let Some(line) = st.ready.pop_front() {
                match (st.parse)(&line) {
                    Ok(WireEvent::Skip) => {}
                    Ok(WireEvent::Delta(t)) => return Some((Ok(t), st)),
                    Ok(WireEvent::Stop) => return None,
                    Err(e) => {
                        st.stopped = true;
                        return Some((Err(e), st));
                    }
                }
            } else if st.eof {
                return None;
            } else {
                match st.body.next().await {
                    None => {
                        st.eof = true;
                        // Flush the remainder: a final line with no `\n`
                        // (NDJSON with no terminal newline, truncated SSE)
                        // also gets interpreted — a hostile truncation ends
                        // in Protocol in the parser, not in silence.
                        if !st.buf.is_empty() {
                            let tail = std::mem::take(&mut st.buf);
                            match into_line(tail) {
                                Ok(line) => st.ready.push_back(line),
                                Err(e) => {
                                    st.stopped = true;
                                    return Some((Err(e), st));
                                }
                            }
                        }
                    }
                    Some(Err(e)) => {
                        st.stopped = true;
                        return Some((Err(transport(&e)), st));
                    }
                    Some(Ok(chunk)) => {
                        st.buf.extend_from_slice(&chunk);
                        if let Err(e) = drain_lines(&mut st.buf, &mut st.ready) {
                            st.stopped = true;
                            return Some((Err(e), st));
                        }
                    }
                }
            }
        }
    })
    .boxed()
}

/// Extracts every COMPLETE line (ending in `\n`) from `buf` into `ready`.
/// Watches the line cap against the unfinished leftover.
fn drain_lines(buf: &mut Vec<u8>, ready: &mut VecDeque<String>) -> Result<(), AiError> {
    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
        let mut line: Vec<u8> = buf.drain(..=pos).collect();
        line.pop(); // the `\n`
        ready.push_back(into_line(line)?);
    }
    if buf.len() > MAX_LINE_BYTES {
        return Err(AiError::Protocol("stream line too long (>1 MiB)".into()));
    }
    Ok(())
}

/// A line of bytes → `String`, trimming ALL trailing `\r`s (some servers
/// emit `\r\r\n`). SSE/NDJSON are UTF-8 by contract: invalid bytes = broken
/// server → [`AiError::Protocol`], never silent lossy conversion.
fn into_line(mut line: Vec<u8>) -> Result<String, AiError> {
    while line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line).map_err(|_| AiError::Protocol("stream line is not UTF-8".into()))
}

#[cfg(test)]
pub(crate) mod testutil {
    //! A hand-rolled HTTP/1.1 fake server over `tokio::net::TcpListener`
    //! (ADR 0031: no new dev-deps, no network in the tests) + tests of the
    //! line buffering itself.

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// A ONE-request fake server: reads it whole, answers with the raw
    /// `response` and closes. The captured request is recovered with
    /// [`FakeHttp::request`].
    pub(crate) struct FakeHttp {
        pub(crate) base_url: String,
        handle: tokio::task::JoinHandle<String>,
    }

    impl FakeHttp {
        /// The raw request the server received (request line + headers +
        /// body). Consume it AFTER the response has been drained.
        pub(crate) async fn request(self) -> String {
            self.handle.await.unwrap()
        }
    }

    /// Starts the fake server on `127.0.0.1:0` and returns its base URL.
    pub(crate) async fn serve_once(response: Vec<u8>) -> FakeHttp {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let req = read_request(&mut sock).await;
            sock.write_all(&response).await.unwrap();
            sock.flush().await.unwrap();
            sock.shutdown().await.ok();
            req
        });
        FakeHttp {
            base_url: format!("http://{addr}"),
            handle,
        }
    }

    /// A MULTI-request fake server: answers the nth response to the nth
    /// request, and returns every request in order.
    ///
    /// Needed to test a RETRY, which by definition is two round trips: the
    /// one the server rejects and the one it accepts. With `serve_once` only
    /// the first can be seen, so the `OpenAI`-compatible provider's typed-
    /// output fallback could not be checked.
    pub(crate) struct FakeHttpN {
        pub(crate) base_url: String,
        handle: tokio::task::JoinHandle<Vec<String>>,
    }

    impl FakeHttpN {
        /// The raw requests, in order. Consume them AFTER the responses have
        /// been drained.
        pub(crate) async fn requests(self) -> Vec<String> {
            self.handle.await.unwrap()
        }
    }

    /// Starts a fake server that serves `responses.len()` requests.
    pub(crate) async fn serve_seq(responses: Vec<Vec<u8>>) -> FakeHttpN {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let mut reqs = Vec::new();
            for response in responses {
                let (mut sock, _) = listener.accept().await.unwrap();
                reqs.push(read_request(&mut sock).await);
                sock.write_all(&response).await.unwrap();
                sock.flush().await.unwrap();
                sock.shutdown().await.ok();
            }
            reqs
        });
        FakeHttpN {
            base_url: format!("http://{addr}"),
            handle,
        }
    }

    /// Builds a raw HTTP/1.1 response with a correct `content-length` and
    /// `connection: close` (one request per connection).
    pub(crate) fn response(
        status: u16,
        reason: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Vec<u8> {
        use std::fmt::Write as _;
        let mut out = format!("HTTP/1.1 {status} {reason}\r\n");
        for (k, v) in headers {
            let _ = write!(out, "{k}: {v}\r\n");
        }
        let _ = write!(out, "content-length: {}\r\n", body.len());
        out.push_str("connection: close\r\n\r\n");
        out.push_str(body);
        out.into_bytes()
    }

    /// Reads a complete request: headers up to `\r\n\r\n` plus
    /// `content-length` bytes of body.
    async fn read_request(sock: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = sock.read(&mut tmp).await.unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                let clen: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                if buf.len() >= end + 4 + clen {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    // ---- line-buffering tests (via a real GET to the fake) ----

    use futures::StreamExt;

    use super::{MAX_LINE_BYTES, WireEvent, check_status, delta_stream};
    use crate::provider::AiError;

    /// Every line is a delta; the tail with no final `\n` is also delivered
    /// and trailing `\r`s (even doubled) are trimmed.
    #[tokio::test]
    async fn lines_with_a_tail_with_no_newline_and_double_cr() {
        let srv = serve_once(response(200, "OK", &[], "a\r\r\nb\nc")).await;
        let resp = reqwest::get(format!("{}/x", srv.base_url)).await.unwrap();
        let s = delta_stream(check_status(resp).unwrap(), |l| {
            Ok(WireEvent::Delta(l.to_string()))
        });
        let got: Vec<String> = s.map(Result::unwrap).collect().await;
        assert_eq!(got, ["a", "b", "c"]);
    }

    /// A giant line with no `\n` (hostile server) cuts the stream off with
    /// `Protocol`, it does not accumulate without limit.
    #[tokio::test]
    async fn a_giant_line_is_protocol() {
        let huge = "x".repeat(MAX_LINE_BYTES + 1);
        let srv = serve_once(response(200, "OK", &[], &huge)).await;
        let resp = reqwest::get(format!("{}/x", srv.base_url)).await.unwrap();
        let mut s = delta_stream(check_status(resp).unwrap(), |_| Ok(WireEvent::Skip));
        let item = s.next().await.unwrap();
        assert!(matches!(item, Err(AiError::Protocol(_))), "{item:?}");
        assert!(s.next().await.is_none(), "the stream ends after the Err");
    }

    /// A `Stop` from the parser ends the stream even if lines remain after it.
    #[tokio::test]
    async fn stop_ends_the_stream() {
        let srv = serve_once(response(200, "OK", &[], "one\nSTOP\nignored\n")).await;
        let resp = reqwest::get(format!("{}/x", srv.base_url)).await.unwrap();
        let s = delta_stream(check_status(resp).unwrap(), |l| {
            if l == "STOP" {
                Ok(WireEvent::Stop)
            } else {
                Ok(WireEvent::Delta(l.to_string()))
            }
        });
        let got: Vec<String> = s.map(Result::unwrap).collect().await;
        assert_eq!(got, ["one"]);
    }
}
