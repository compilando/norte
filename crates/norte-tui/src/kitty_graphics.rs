//! ¿Sabe este terminal pintar gráficos por el protocolo de kitty? La misma
//! pregunta que [`crate::alt_menu`] le hace al protocolo de TECLADO, para el
//! protocolo de IMÁGENES: se pregunta una vez, al arrancar, y se guarda.
//! Nadie pinta nada aquí — eso es de otra tarea; esta sólo abre o cierra la
//! puerta.
//!
//! El protocolo se describe en
//! <https://sw.kovidgoyal.net/kitty/graphics-protocol/>: una secuencia APC
//! (`\x1b_G…\x1b\\`) que un terminal que no lo habla simplemente IGNORA, sin
//! contestar nada. Por eso la sonda manda DETRÁS un DA1 (`\x1b[c`), que todo
//! terminal VT100-compatible sí contesta: sin él no habría nada que esperar
//! y la sonda agotaría el plazo en cada terminal sin soporte.

use std::io::{self, IsTerminal, Read, Write};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// El id con el que se pregunta. Arbitrario y sólo nuestro: una respuesta
/// con otro id contesta a otra pregunta y no dice nada de la nuestra.
const ID_SONDA: &str = "i=31";

/// La consulta: una imagen de 1x1 en RGB (`f=24`) transmitida inline
/// (`t=d`), con acción `a=q` —"query", nunca dibuja nada— y DETRÁS un DA1
/// para tener algo que esperar en un terminal que no conteste al APC.
const QUERY: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c";

/// Cuánto se espera la respuesta antes de darla por «no».
const PLAZO: Duration = Duration::from_millis(200);

/// ¿La contestación del terminal dice que sabe pintar gráficos?
///
/// Se busca la respuesta APC del protocolo (`\x1b_G…;OK\x1b\\`) CON NUESTRO
/// ID. Cualquier otra cosa —sólo la respuesta de DA1, un error declarado,
/// nada en absoluto— es «no»: quien no sabe, calla.
fn respuesta_dice_si(bytes: &[u8]) -> bool {
    let Ok(texto) = std::str::from_utf8(bytes) else {
        return false;
    };
    texto
        .split("\x1b_G")
        .skip(1)
        .any(|resto| match resto.split_once("\x1b\\") {
            Some((cuerpo, _)) => cuerpo.contains(ID_SONDA) && cuerpo.ends_with(";OK"),
            None => false,
        })
}

/// Lo que contestó el terminal, preguntado UNA vez.
static SOPORTE: OnceLock<bool> = OnceLock::new();

/// Pregunta al terminal si sabe pintar gráficos, y guarda la respuesta.
///
/// Se llama al ARRANCAR, con raw mode ya puesto y antes de que el bucle
/// levante su lector de eventos, por los dos motivos que ya documenta
/// `alt_menu::consultar_soporte`: con el lector vivo, ese hilo tiene el lock
/// y la pregunta se rinde; y bajo `--pick` stdout es la tubería de datos de
/// quien llama, así que sin terminal en stdout no se pregunta.
///
/// NO tiene test: lo que hace es escribir en la terminal de control y leerla
/// con un plazo. Lo testeable es [`respuesta_dice_si`], que sí lo está. Un
/// test de esto necesitaría un pty falso que contestara como kitty, y eso es
/// probar el pty.
///
/// Se manda el query APC y DETRÁS un DA1: un terminal que no habla el
/// protocolo ignora el primero en silencio, y sin el segundo no habría nada
/// que esperar — la sonda agotaría el plazo siempre, y el arranque pagaría
/// ese plazo en cada terminal que no lo soporta.
pub fn consultar_soporte() -> bool {
    *SOPORTE.get_or_init(|| {
        if !io::stdout().is_terminal() {
            return false;
        }
        preguntar().unwrap_or(false)
    })
}

/// La respuesta de [`consultar_soporte`], sin preguntar. Si no se preguntó,
/// «no».
#[must_use]
pub fn soportado() -> bool {
    SOPORTE.get().copied().unwrap_or(false)
}

/// Escribe la consulta en `/dev/tty` y lee la respuesta con un plazo corto,
/// hasta ver la `c` que cierra el DA1 o hasta agotar [`PLAZO`].
///
/// Un fallo al abrir o escribir la terminal de control se lee como «no»
/// desde [`consultar_soporte`]: una sonda de presentación no tumba el
/// arranque.
///
/// `/dev/tty` no tiene un `read_timeout` como un socket (regla 5: el `poll`
/// de verdad es `unsafe`, y ese `unsafe` es sólo de `norte-vfs-local`), así
/// que la lectura corre en un hilo aparte y el que pregunta espera con
/// [`std::sync::mpsc::Receiver::recv_timeout`]. Si el plazo vence, el hilo
/// se queda leyendo hasta que el terminal cierre o conteste — huérfano pero
/// inofensivo: el proceso no espera por él, y muere con el proceso.
fn preguntar() -> io::Result<bool> {
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")?;
    tty.write_all(QUERY)?;
    tty.flush()?;
    let mut lector = tty.try_clone()?;

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match lector.read(&mut byte) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let c = byte[0];
                    buf.push(c);
                    let cierra = c == b'c';
                    if tx.send(buf.clone()).is_err() || cierra {
                        break;
                    }
                }
            }
        }
    });

    let limite = Instant::now() + PLAZO;
    let mut leido = Vec::new();
    while let Some(restante) = limite.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(restante) {
            Ok(buf) => leido = buf,
            Err(_) => break,
        }
    }
    let soporte = respuesta_dice_si(&leido);
    // Sin esto, un «no» y un terminal que no contestó nada son
    // indistinguibles desde fuera. Es la evidencia del paso 6 (la puerta):
    // qué contestó cada terminal de verdad, no sólo el sí/no final.
    tracing::debug!(
        respuesta = %String::from_utf8_lossy(&leido).escape_debug(),
        soporte,
        "sonda de gráficos de kitty"
    );
    Ok(soporte)
}

#[cfg(test)]
mod tests {
    use super::respuesta_dice_si;

    #[test]
    fn una_respuesta_de_kitty_es_que_si() {
        // kitty contesta al query con OK para el id que se le mandó.
        assert!(respuesta_dice_si(b"\x1b_Gi=31;OK\x1b\\\x1b[?62;c"));
    }

    #[test]
    fn solo_la_respuesta_de_da1_es_que_no() {
        // Un terminal que no habla el protocolo ignora el APC y sólo
        // contesta a DA1. Es el caso de xterm, de VTE y de tmux sin
        // passthrough, y es la razón de mandar DA1 detrás: sin él no habría
        // nada que esperar y la sonda colgaría hasta el plazo.
        assert!(!respuesta_dice_si(b"\x1b[?62;c"));
    }

    #[test]
    fn un_ok_de_otro_id_no_cuenta() {
        // Si la respuesta es de otra consulta (un id que no es el nuestro),
        // no dice nada de nuestra pregunta.
        assert!(!respuesta_dice_si(b"\x1b_Gi=99;OK\x1b\\\x1b[?62;c"));
    }

    #[test]
    fn un_error_declarado_es_que_no() {
        assert!(!respuesta_dice_si(b"\x1b_Gi=31;ENOTSUPPORTED\x1b\\"));
    }

    #[test]
    fn nada_es_que_no() {
        assert!(!respuesta_dice_si(b""));
    }
}
