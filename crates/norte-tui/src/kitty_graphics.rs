//! ¿Sabe este terminal pintar gráficos por el protocolo de kitty? La misma
//! pregunta que [`crate::alt_menu`] le hace al protocolo de TECLADO, para el
//! protocolo de IMÁGENES: se pregunta una vez, al arrancar, y se guarda.
//!
//! T4 (fase 5 WOW) añade lo que sí pinta: [`escape_colocar`]/[`escape_borrar`]
//! son los escapes puros —nada de I/O aquí, eso corre en el run loop, dueño de
//! la terminal—, y [`marcar_colocada`]/[`borrar_colocada`] llevan la cuenta de
//! qué id hay puesto AHORA MISMO en la terminal de verdad. Esa cuenta es
//! estado de PROCESO, como `alt_menu::PEDIDO` (privado, sin enlazar desde
//! aquí — el mismo motivo que documenta [`consultar_soporte`] más abajo):
//! ceder la terminal, un `Esc` que cierra el visor y la salida necesitan
//! poder borrar SIN que nadie les pase el `App` — el `App` decide QUÉ se
//! quiere ver, esto lleva la cuenta de qué hay de verdad en pantalla.
//!
//! El protocolo se describe en
//! <https://sw.kovidgoyal.net/kitty/graphics-protocol/>: una secuencia APC
//! (`\x1b_G…\x1b\\`) que un terminal que no lo habla simplemente IGNORA, sin
//! contestar nada. Por eso la sonda manda DETRÁS un DA1 (`\x1b[c`), que todo
//! terminal VT100-compatible sí contesta: sin él no habría nada que esperar
//! y la sonda agotaría el plazo en cada terminal sin soporte.

use std::io::{self, IsTerminal, Read, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use base64::Engine as _;
use ratatui::layout::Rect;

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
            Some((cuerpo, _)) => es_nuestro_ok(cuerpo),
            None => false,
        })
}

/// `cuerpo` es lo de entre `\x1b_G` y `\x1b\\`: claves separadas por comas
/// (`i=31`, `I=2`…) y DESPUÉS un `;` el mensaje (`OK`, `ENOTSUPPORTED`…).
///
/// Revisión, hallazgo 1: la primera versión comprobaba `contains(ID_SONDA)`,
/// una subcadena — y `"i=311;OK".contains("i=31")` es cierto, así que la
/// respuesta a OTRA consulta (id 311, no 31) contaba como un sí para la
/// nuestra. Aquí se separa el campo de claves del mensaje por el primer
/// `;`, y se compara CADA clave por IGUALDAD exacta contra [`ID_SONDA`]:
/// ningún id que sólo comparta prefijo cuela.
fn es_nuestro_ok(cuerpo: &str) -> bool {
    let Some((claves, mensaje)) = cuerpo.split_once(';') else {
        return false;
    };
    mensaje == "OK" && claves.split(',').any(|clave| clave == ID_SONDA)
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
/// con un plazo. Lo testeable es el parseo (`respuesta_dice_si`, privada —
/// sin corchetes: enlazar desde aquí, que es público, a un ítem privado es
/// un `rustdoc::private_intra_doc_links` denegado en el gate), que sí lo
/// está. Un test de esto necesitaría un pty falso que contestara como
/// kitty, y eso es probar el pty.
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

/// Cuántos bytes CRUDOS (antes de base64) lleva cada trozo de un APC.
///
/// El protocolo trocea por longitud del payload YA en base64 (4096
/// caracteres es el límite de kitty), así que se trocea en crudo en un
/// múltiplo de 3: 3 bytes crudos son exactamente 4 caracteres de base64, sin
/// relleno a mitad de trozo. `3 * 1024` bytes crudos = 4096 caracteres,
/// justo el límite.
const CHUNK_RAW_BYTES: usize = 3 * 1024;

/// El escape que coloca la imagen en el hueco del visor.
///
/// `a=T` transmite Y muestra de una vez, en la posición del CURSOR —el
/// llamante lo coloca en `rect` antes de escribir esto (T4, el run loop).
/// `f=100` es PNG, que es lo que devuelve el kind `thumbnail`. Los bytes van
/// en base64 porque un APC termina en `\x1b\\` y un PNG contiene esa pareja
/// con toda normalidad: mandarlo crudo cortaría la imagen por la mitad y
/// dejaría el resto escrito en la pantalla como texto.
///
/// `c`/`r` son CELDAS, no píxeles: se le dice al terminal el HUECO y él
/// encaja, que es lo que mantiene la imagen dentro del marco cuando el
/// terminal tiene celdas de otro tamaño del que supusimos.
///
/// Si `bytes` pasa de `CHUNK_RAW_BYTES` (privado, sin enlazar) se trocea en
/// varios APC seguidos:
/// el primero lleva TODA la cabecera (`i`, `f`, `c`, `r`) más `m=1`; los
/// siguientes sólo llevan `m` (`1` mientras queden más, `0` en el último) —
/// es lo normal, porque una miniatura de verdad (hasta 1920 px de lado) no
/// cabe nunca en un único trozo.
///
/// ```
/// use norte_tui::kitty_graphics::escape_colocar;
/// use ratatui::layout::Rect;
///
/// let esc = escape_colocar(7, b"PNGFALSO", Rect::new(1, 2, 40, 20));
/// assert!(esc.starts_with("\x1b_G") && esc.ends_with("\x1b\\"));
/// ```
#[must_use]
pub fn escape_colocar(id: u32, bytes: &[u8], rect: Rect) -> String {
    let engine = base64::engine::general_purpose::STANDARD;
    // `chunks` de un slice vacío no produce ningún trozo, y una miniatura de
    // cero bytes sigue necesitando UN APC (vacío) para que el terminal la
    // reconozca — de ahí el `[&[][..]]` de respaldo.
    let trozos: Vec<&[u8]> = if bytes.is_empty() {
        vec![&[]]
    } else {
        bytes.chunks(CHUNK_RAW_BYTES).collect()
    };
    let total = trozos.len();
    let mut out = String::new();
    for (i, trozo) in trozos.into_iter().enumerate() {
        use std::fmt::Write as _;
        let ultimo = i + 1 == total;
        let mas = u8::from(!ultimo);
        out.push_str("\x1b_G");
        if i == 0 {
            // `write!` en un `String` no falla nunca (regla 6: no hay
            // `unwrap`/`expect` fuera de test, y aquí no hace falta ni eso).
            let _ = write!(
                out,
                "a=T,i={id},f=100,c={},r={},m={mas}",
                rect.width, rect.height
            );
        } else {
            let _ = write!(out, "m={mas}");
        }
        out.push(';');
        out.push_str(&engine.encode(trozo));
        out.push_str("\x1b\\");
    }
    out
}

/// El escape que borra SÓLO esta imagen.
///
/// `d=i` borra POR ID. Sin el id se borrarían las imágenes de todo el
/// terminal, incluidas las de otro programa en otra pestaña.
///
/// ```
/// use norte_tui::kitty_graphics::escape_borrar;
/// assert!(escape_borrar(7).contains("i=7"));
/// ```
#[must_use]
pub fn escape_borrar(id: u32) -> String {
    format!("\x1b_Ga=d,d=i,i={id}\x1b\\")
}

/// El id de la imagen que está colocada AHORA MISMO en la terminal de
/// verdad, o `0` si no hay ninguna — estado de PROCESO, como
/// [`crate::alt_menu`]'s `PEDIDO`/`CEDIDO`: `0` no es un id válido porque
/// [`crate::viewer_open::ImagenColocada::id`] arranca en 1, así que sirve de
/// centinela sin envolver en `Option` un átomo.
static COLOCADA: AtomicU32 = AtomicU32::new(0);

/// Anota que `id` se acaba de colocar en la terminal de verdad.
///
/// Lo llama el run loop justo después de escribir [`escape_colocar`] con
/// éxito — nunca antes, o un fallo de escritura dejaría esta cuenta
/// creyendo puesta una imagen que la terminal nunca vio.
pub fn marcar_colocada(id: u32) {
    COLOCADA.store(id, Ordering::Relaxed);
}

/// Borra la imagen colocada AHORA MISMO, si hay alguna, y olvida cuál era.
///
/// Idempotente —llamar dos veces seguidas la segunda no escribe nada—, y
/// nunca falla hacia el llamante: un escape que no se pudo escribir se traga
/// con un `tracing::debug!` (regla del pintado: una imagen que no se borra
/// es una molestia, no un motivo para tumbar la TUI ni la suspensión).
///
/// Es el punto de borrado COMPARTIDO por los cuatro momentos (T4): cerrar el
/// visor o moverlo a otro fichero lo alcanzan por la diferencia que hace el
/// run loop cada frame (compara el id deseado contra este); ceder la
/// terminal ([`crate::suspend::suspend_terminal`]) y salir
/// ([`crate::tty::restore`]) lo llaman aquí directamente porque ninguno de
/// los dos tiene garantizado un frame siguiente que haga esa diferencia.
pub fn borrar_colocada(out: &mut impl Write) {
    let id = COLOCADA.swap(0, Ordering::Relaxed);
    if id == 0 {
        return;
    }
    match out
        .write_all(escape_borrar(id).as_bytes())
        .and_then(|()| out.flush())
    {
        Ok(()) => {}
        Err(e) => tracing::debug!(error = %e, id, "no se pudo borrar la imagen colocada"),
    }
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
/// [`std::sync::mpsc::Receiver::recv_timeout`].
///
/// **Revisión, hallazgo 2 — por qué el hilo sigue leyendo tras el plazo, en
/// vez de tirar la toalla con él.** El primer diseño comprobaba, byte a
/// byte, si el que pregunta seguía escuchando, y se rendía en cuanto dejaba
/// de estarlo. Eso significa que una respuesta TARDÍA (SSH con latencia
/// real) se leía UN byte —el que hacía fallar el envío— y el resto
/// (`[?62;c`) se dejaba sin consumir en la terminal, esperando a que el
/// lector de eventos de crossterm arrancara y se lo comiera como
/// pulsaciones del usuario. Aquí el hilo, una vez lanzado, YA NO comprueba
/// si alguien escucha: sigue leyendo hasta ver la `c` (o un error/EOF) pase
/// lo que pase, y sólo entonces intenta mandar el resultado — que si el
/// plazo ya venció, nadie recoge, y no importa: el trabajo del hilo nunca
/// fue avisar a quien se rindió, es DRENAR la respuesta entera de la
/// terminal antes de que otro lector la confunda con teclado.
///
/// Esto no cierra la ventana del todo. Sigue existiendo una carrera real:
/// si la respuesta tarda tanto que el lector de eventos ya arrancó (más
/// allá de este arranque, dentro de `run`) ANTES de que este hilo termine
/// de leerla, los dos compiten por los mismos bytes del mismo fd y cuál se
/// queda con cada uno no está definido. Cerrarla del todo pediría o bien
/// esperar indefinidamente aquí (perder la garantía de plazo corto que es
/// el motivo de esta sonda) o vaciar el búfer de entrada de la terminal
/// (`tcflush`, que es `unsafe`/`libc` — la regla 5 lo prohíbe fuera de
/// `norte-vfs-local`). Se acepta el riesgo residual, acotado a terminales
/// con latencia mucho mayor que [`PLAZO`] (200 ms) Y que además tardan tanto
/// en contestar que alcanzan a solaparse con el arranque del lector de
/// eventos — no observado en las pruebas locales (tmux, kitty) de esta
/// tarea.
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
                    if c == b'c' {
                        break;
                    }
                }
            }
        }
        // Best-effort: si el que preguntó ya no escucha (el plazo venció),
        // el envío falla y se ignora — para entonces el drenado de arriba
        // ya hizo lo que importaba.
        let _ = tx.send(buf);
    });

    let leido = rx.recv_timeout(PLAZO).unwrap_or_default();
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
    fn un_id_que_comparte_prefijo_no_cuenta() {
        // Revisión, hallazgo 1: "i=311" CONTIENE "i=31" como subcadena y
        // también termina en ";OK" — un id ajeno que por casualidad
        // comparte prefijo no puede colarse como si fuera el nuestro.
        assert!(!respuesta_dice_si(b"\x1b_Gi=311;OK\x1b\\"));
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
