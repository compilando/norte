//! Helpers HTTP compartidos por los proveedores (ADR 0031): mapeo de status
//! HTTP → [`AiError`], y el stream de líneas (SSE/NDJSON) montado sobre el
//! body de reqwest. Cada proveedor aporta SOLO el parser de una línea; el
//! buffering, el corte en `\n` y la terminación viven aquí.

use std::collections::VecDeque;

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;

use crate::provider::{AiError, ChatRequest, ChatRole, ChatStream};

/// Tope de UNA línea del wire (SSE/NDJSON). Un delta de chat jamás se acerca;
/// pasarse = server hostil o roto → [`AiError::Protocol`] (ADR 0031: los
/// casos hostiles se cortan tipados, no se acumulan sin límite).
const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Resultado de interpretar UNA línea del wire.
pub(crate) enum WireEvent {
    /// Delta de texto a entregar al consumidor.
    Delta(String),
    /// Línea sin interés (ping, `event:`, keep-alive, chunk sin contenido).
    Skip,
    /// Fin limpio de la respuesta (`message_stop`, `[DONE]`, `done: true`).
    Stop,
}

/// Mapea el status de una respuesta YA recibida: 401/403 → [`AiError::Auth`],
/// 429 → [`AiError::RateLimited`] (con `retry-after` en segundos si vino),
/// resto no-2xx → [`AiError::Http`]. Una 2xx pasa intacta.
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

/// Error de transporte (conexión, DNS, TLS, timeout). El `Display` de
/// `reqwest::Error` no incluye headers: la api key jamás se filtra por aquí
/// (regla 10).
pub(crate) fn transport(e: &reqwest::Error) -> AiError {
    AiError::Transport(e.to_string())
}

/// El payload de una línea SSE `data: ...` (sin el prefijo ni el espacio
/// opcional); `None` si la línea no es de datos (`event:`, comentario, vacía).
pub(crate) fn sse_data(line: &str) -> Option<&str> {
    line.strip_prefix("data:").map(str::trim_start)
}

/// Valida el contrato de [`ChatRequest`]: existe al menos un turno no-system
/// y el primero es `user` (lo exigen los tres proveedores; se corta aquí,
/// tipado, antes de tocar la red).
pub(crate) fn validate_turns(req: &ChatRequest) -> Result<(), AiError> {
    match req.messages.iter().find(|m| m.role != ChatRole::System) {
        Some(m) if m.role == ChatRole::User => Ok(()),
        _ => Err(AiError::Protocol(
            "el primer turno de la conversación debe ser `user`".into(),
        )),
    }
}

/// Estado del stream de líneas: posee el body de reqwest (dropear el stream
/// devuelto dropea el body → reqwest aborta la petición HTTP; regla 3,
/// cancelación drop-based).
struct LineState<F> {
    body: BoxStream<'static, reqwest::Result<Bytes>>,
    buf: Vec<u8>,
    ready: VecDeque<String>,
    eof: bool,
    stopped: bool,
    parse: F,
}

/// Stream de deltas sobre el body de `resp`: bufferiza bytes, corta en `\n`
/// (recortando TODOS los `\r` finales), y pasa cada línea completa por
/// `parse`. Un `Err` del parser o del transporte TERMINA el stream tras
/// emitirse; [`WireEvent::Stop`] lo termina sin emitir nada más. El resto de
/// bytes al EOF (línea final sin `\n`) también pasa por el parser.
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
                        // Flush del resto: una línea final sin `\n` (NDJSON
                        // sin newline terminal, SSE truncado) también se
                        // interpreta — un truncado hostil acaba en Protocol
                        // en el parser, no en silencio.
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

/// Extrae de `buf` todas las líneas COMPLETAS (terminadas en `\n`) hacia
/// `ready`. Vigila el tope de línea sobre el residuo sin terminar.
fn drain_lines(buf: &mut Vec<u8>, ready: &mut VecDeque<String>) -> Result<(), AiError> {
    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
        let mut line: Vec<u8> = buf.drain(..=pos).collect();
        line.pop(); // el `\n`
        ready.push_back(into_line(line)?);
    }
    if buf.len() > MAX_LINE_BYTES {
        return Err(AiError::Protocol(
            "línea del stream demasiado larga (>1 MiB)".into(),
        ));
    }
    Ok(())
}

/// Una línea de bytes → `String`, recortando TODOS los `\r` finales (hay
/// servers que emiten `\r\r\n`). SSE/NDJSON son UTF-8 por contrato: bytes
/// inválidos = server roto → [`AiError::Protocol`], jamás lossy silencioso.
fn into_line(mut line: Vec<u8>) -> Result<String, AiError> {
    while line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line).map_err(|_| AiError::Protocol("línea del stream no es UTF-8".into()))
}

#[cfg(test)]
pub(crate) mod testutil {
    //! Fake server HTTP/1.1 a mano sobre `tokio::net::TcpListener` (ADR 0031:
    //! sin dev-deps nuevas, sin red en los tests) + tests del propio buffering
    //! de líneas.

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// Un fake server de UNA petición: la lee entera, responde `response`
    /// crudo y cierra. La petición capturada se recupera con
    /// [`FakeHttp::request`].
    pub(crate) struct FakeHttp {
        pub(crate) base_url: String,
        handle: tokio::task::JoinHandle<String>,
    }

    impl FakeHttp {
        /// La petición cruda que recibió el server (request line + headers +
        /// body). Consúmela DESPUÉS de agotar la respuesta.
        pub(crate) async fn request(self) -> String {
            self.handle.await.unwrap()
        }
    }

    /// Arranca el fake server en `127.0.0.1:0` y devuelve su URL base.
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

    /// Construye una respuesta HTTP/1.1 cruda con `content-length` correcto y
    /// `connection: close` (una petición por conexión).
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

    /// Lee una petición completa: headers hasta `\r\n\r\n` + `content-length`
    /// bytes de body.
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

    // ---- tests del buffering de líneas (vía un GET real al fake) ----

    use futures::StreamExt;

    use super::{MAX_LINE_BYTES, WireEvent, check_status, delta_stream};
    use crate::provider::AiError;

    /// Cada línea es un delta; la cola sin `\n` final también se entrega y
    /// los `\r` finales (incluso dobles) se recortan.
    #[tokio::test]
    async fn lineas_con_cola_sin_newline_y_cr_dobles() {
        let srv = serve_once(response(200, "OK", &[], "a\r\r\nb\nc")).await;
        let resp = reqwest::get(format!("{}/x", srv.base_url)).await.unwrap();
        let s = delta_stream(check_status(resp).unwrap(), |l| {
            Ok(WireEvent::Delta(l.to_string()))
        });
        let got: Vec<String> = s.map(Result::unwrap).collect().await;
        assert_eq!(got, ["a", "b", "c"]);
    }

    /// Una línea gigante sin `\n` (server hostil) corta el stream con
    /// `Protocol`, no acumula sin límite.
    #[tokio::test]
    async fn linea_gigante_es_protocol() {
        let huge = "x".repeat(MAX_LINE_BYTES + 1);
        let srv = serve_once(response(200, "OK", &[], &huge)).await;
        let resp = reqwest::get(format!("{}/x", srv.base_url)).await.unwrap();
        let mut s = delta_stream(check_status(resp).unwrap(), |_| Ok(WireEvent::Skip));
        let item = s.next().await.unwrap();
        assert!(matches!(item, Err(AiError::Protocol(_))), "{item:?}");
        assert!(s.next().await.is_none(), "el stream termina tras el Err");
    }

    /// `Stop` del parser termina el stream aunque queden líneas detrás.
    #[tokio::test]
    async fn stop_termina_el_stream() {
        let srv = serve_once(response(200, "OK", &[], "uno\nSTOP\nignorado\n")).await;
        let resp = reqwest::get(format!("{}/x", srv.base_url)).await.unwrap();
        let s = delta_stream(check_status(resp).unwrap(), |l| {
            if l == "STOP" {
                Ok(WireEvent::Stop)
            } else {
                Ok(WireEvent::Delta(l.to_string()))
            }
        });
        let got: Vec<String> = s.map(Result::unwrap).collect().await;
        assert_eq!(got, ["uno"]);
    }
}
