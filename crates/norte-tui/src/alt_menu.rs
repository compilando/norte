//! Alt pulsado y soltado SOLO abre la barra de menús (`[ui] alt_menu`).
//!
//! Un terminal no avisa de un modificador suelto: en la codificación de
//! siempre, Alt solo no produce ningún byte. Solo el protocolo de teclado de
//! kitty lo cuenta, y únicamente en su modo «report all keys» (flag 8), con
//! los tipos de evento (flag 2) para saber cuándo se SUELTA. Lo implementan
//! kitty, foot, `WezTerm` y Ghostty; tmux, xterm y los terminales de VTE no, y
//! ahí esta clave no hace nada.
//!
//! Por qué va apagada por defecto: en ese modo el terminal manda la TECLA y
//! no el texto, y crossterm no lee el texto asociado (flag 16). Una letra
//! compuesta con tecla muerta —la `é` de un teclado español— o un símbolo
//! escrito con `AltGr` —`@`, `#`, `[`— llega como su tecla base: en un
//! renombrado, `a@b` puede quedar `a2b`. El flag 4 solo arregla Shift, porque
//! `AltGr` no es un modificador del protocolo. Quien la encienda lo hace
//! sabiendo eso.
//!
//! Dos piezas, y ninguna decide qué hace el menú:
//! - [`set`] pide o retira el protocolo. Su estado es de PROCESO, como el raw
//!   mode, para que suspender, salir y el hook de pánico lo devuelvan sin que
//!   nadie tenga que pasarles nada.
//! - [`AltSolo`] reconoce el gesto sobre los eventos que llegan.

use std::io::{self, IsTerminal, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, ModifierKeyCode,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};

/// ¿Está pedido el protocolo ahora mismo en el terminal?
static PEDIDO: AtomicBool = AtomicBool::new(false);

/// Desambiguar (1), tipos de evento (2), teclas alternativas (4) y todas las
/// teclas como escape (8).
///
/// El 4 no es opcional: sin él, con el 8 puesto, `Shift+a` llega como la
/// tecla `a` con SHIFT, y el adaptador del keymap —que confía en que un
/// `Char` ya trae la mayúscula— leería una minúscula. Con el 4 crossterm usa
/// la tecla desplazada y quita el SHIFT, que es la forma de siempre.
const FLAGS: KeyboardEnhancementFlags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
    .union(KeyboardEnhancementFlags::REPORT_EVENT_TYPES)
    .union(KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS)
    .union(KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES);

/// Pide (o retira) el protocolo si hace falta. Idempotente, como
/// `mouse::Capture::set`: la recarga de config lo llama en cada vuelta.
///
/// `soportado` solo se consulta al ENCENDER, y es una función para que los
/// tests no pregunten a un terminal que no hay. Un terminal que no lo soporta
/// no recibe nada y la clave se queda sin efecto, sin error: no hay nada que
/// el lector pueda arreglar desde norte.
///
/// # Errors
/// La de escribir en `out`.
pub fn set(want: bool, soportado: impl FnOnce() -> bool, out: &mut impl Write) -> io::Result<()> {
    if want == PEDIDO.load(Ordering::Relaxed) {
        return Ok(());
    }
    if want {
        if !soportado() {
            return Ok(());
        }
        crossterm::execute!(out, PushKeyboardEnhancementFlags(FLAGS))?;
    } else if !CEDIDO.swap(false, Ordering::Relaxed) {
        // Cedido ya está fuera de la pila: quitarlo otra vez se llevaría una
        // entrada que es del shell.
        crossterm::execute!(out, PopKeyboardEnhancementFlags)?;
    }
    PEDIDO.store(want, Ordering::Relaxed);
    Ok(())
}

/// Pedido, pero retirado mientras otro programa tiene la terminal. Es lo que
/// empareja [`ceder`] con [`recuperar`]: sin esto, un fallo ANTES de ceder
/// dejaba a `recuperar` apilando una segunda entrada que la salida no quita.
static CEDIDO: AtomicBool = AtomicBool::new(false);

/// Lo que contestó el terminal, preguntado UNA vez.
static SOPORTE: OnceLock<bool> = OnceLock::new();

/// Pregunta al terminal si habla el protocolo, y guarda la respuesta.
///
/// Se llama al ARRANCAR, antes de que el bucle levante su lector de eventos,
/// y en ningún otro momento. Dos motivos, y los dos cuestan caro:
/// - con el lector vivo, ese hilo tiene el lock de crossterm; la pregunta
///   espera dos segundos, se rinde y contesta «no». Una recarga de config
///   congelaba la TUI dos segundos y dejaba el gesto apagado.
/// - crossterm manda la pregunta por STDOUT si no puede escribir en la
///   terminal de control, y bajo `--pick` stdout es la tubería de datos de
///   quien llama. Sin un terminal en stdout no se pregunta: se da por «no».
///
/// Un error se lee como «no»: una clave de presentación no tumba el arranque.
pub fn consultar_soporte() -> bool {
    *SOPORTE.get_or_init(|| {
        io::stdout().is_terminal()
            && crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false)
    })
}

/// La respuesta de [`consultar_soporte`], sin preguntar. Si no se preguntó,
/// «no».
#[must_use]
pub fn soportado() -> bool {
    SOPORTE.get().copied().unwrap_or(false)
}

/// Devuelve el terminal a la codificación de siempre SIN olvidar que estaba
/// pedido: para ceder la terminal a un shell o a un editor, que no hablan
/// este protocolo y recibirían escapes en vez de letras. Dos veces seguidas
/// ceden una.
///
/// # Errors
/// La de escribir en `out`.
pub fn ceder(out: &mut impl Write) -> io::Result<()> {
    if PEDIDO.load(Ordering::Relaxed) && !CEDIDO.swap(true, Ordering::Relaxed) {
        crossterm::execute!(out, PopKeyboardEnhancementFlags)?;
    }
    Ok(())
}

/// La pareja de [`ceder`]: al volver de una suspensión. Solo apila lo que se
/// cedió.
///
/// # Errors
/// La de escribir en `out`.
pub fn recuperar(out: &mut impl Write) -> io::Result<()> {
    if PEDIDO.load(Ordering::Relaxed) && CEDIDO.swap(false, Ordering::Relaxed) {
        crossterm::execute!(out, PushKeyboardEnhancementFlags(FLAGS))?;
    }
    Ok(())
}

/// Lo retira desde el hook de pánico, OLVIDANDO que estaba pedido.
///
/// Un pánico dentro de una tarea de tokio corre el hook y el proceso sigue
/// vivo; al salir, `tty::restore` quitaría el protocolo otra vez, ya fuera de
/// la pantalla alternativa, y se llevaría una entrada de la pila del shell.
///
/// # Errors
/// La de escribir en `out`.
pub fn soltar_en_panico(out: &mut impl Write) -> io::Result<()> {
    let pedido = PEDIDO.swap(false, Ordering::Relaxed);
    let cedido = CEDIDO.swap(false, Ordering::Relaxed);
    if pedido && !cedido {
        crossterm::execute!(out, PopKeyboardEnhancementFlags)?;
    }
    Ok(())
}

/// ¿Es la pulsación de una tecla modificadora sola? Esas no son teclas para
/// el keymap: dejarlas pasar resetearía una secuencia a medias (`g` … `g`)
/// en cuanto el lector rozara Alt o Shift.
#[must_use]
pub const fn es_modificador(ev: &KeyEvent) -> bool {
    matches!(ev.code, KeyCode::Modifier(_))
}

/// El gesto: Alt baja sin otro modificador y sube sin nada en medio.
#[derive(Debug, Default)]
pub struct AltSolo {
    armado: bool,
}

impl AltSolo {
    /// Un evento de teclado. `true` = acaba de completarse el gesto.
    ///
    /// Cualquier otra pulsación desarma, así que `Alt+x` no abre el menú al
    /// soltar Alt. La repetición de Alt mantenido no desarma. `AltGr` no es
    /// Alt: el terminal lo manda como `IsoLevel3Shift`, y en un teclado
    /// español escribe `@` y `#`.
    pub fn tecla(&mut self, ev: &KeyEvent) -> bool {
        let es_alt = matches!(
            ev.code,
            KeyCode::Modifier(ModifierKeyCode::LeftAlt | ModifierKeyCode::RightAlt)
        );
        match ev.kind {
            KeyEventKind::Press => {
                self.armado = es_alt && ev.modifiers.difference(KeyModifiers::ALT).is_empty();
                false
            }
            KeyEventKind::Repeat => {
                if !es_alt {
                    self.armado = false;
                }
                false
            }
            KeyEventKind::Release => {
                if !es_alt {
                    return false;
                }
                std::mem::take(&mut self.armado)
            }
        }
    }

    /// Algo que no es teclado se metió en medio: un clic, la rueda.
    pub const fn soltar(&mut self) {
        self.armado = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;

    fn ev(code: KeyCode, kind: KeyEventKind, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind,
            state: KeyEventState::NONE,
        }
    }

    fn alt(kind: KeyEventKind) -> KeyEvent {
        // crossterm pone ALT en los modificadores del propio Alt.
        ev(
            KeyCode::Modifier(ModifierKeyCode::LeftAlt),
            kind,
            KeyModifiers::ALT,
        )
    }

    #[test]
    fn bajar_y_soltar_alt_es_el_gesto() {
        let mut a = AltSolo::default();
        assert!(!a.tecla(&alt(KeyEventKind::Press)));
        assert!(a.tecla(&alt(KeyEventKind::Release)));
    }

    #[test]
    fn alt_mantenido_que_se_repite_sigue_siendo_el_gesto() {
        let mut a = AltSolo::default();
        a.tecla(&alt(KeyEventKind::Press));
        a.tecla(&alt(KeyEventKind::Repeat));
        assert!(a.tecla(&alt(KeyEventKind::Release)));
    }

    #[test]
    fn alt_mas_otra_tecla_no_lo_es() {
        let mut a = AltSolo::default();
        a.tecla(&alt(KeyEventKind::Press));
        a.tecla(&ev(
            KeyCode::Char('x'),
            KeyEventKind::Press,
            KeyModifiers::ALT,
        ));
        a.tecla(&ev(
            KeyCode::Char('x'),
            KeyEventKind::Release,
            KeyModifiers::ALT,
        ));
        assert!(!a.tecla(&alt(KeyEventKind::Release)));
    }

    #[test]
    fn altgr_no_es_alt() {
        let mut a = AltSolo::default();
        let altgr = |k| {
            ev(
                KeyCode::Modifier(ModifierKeyCode::IsoLevel3Shift),
                k,
                KeyModifiers::NONE,
            )
        };
        a.tecla(&altgr(KeyEventKind::Press));
        assert!(!a.tecla(&altgr(KeyEventKind::Release)));
    }

    #[test]
    fn con_ctrl_bajado_no_se_arma() {
        let mut a = AltSolo::default();
        a.tecla(&ev(
            KeyCode::Modifier(ModifierKeyCode::LeftAlt),
            KeyEventKind::Press,
            KeyModifiers::ALT | KeyModifiers::CONTROL,
        ));
        assert!(!a.tecla(&alt(KeyEventKind::Release)));
    }

    #[test]
    fn un_clic_en_medio_lo_desarma() {
        let mut a = AltSolo::default();
        a.tecla(&alt(KeyEventKind::Press));
        a.soltar();
        assert!(!a.tecla(&alt(KeyEventKind::Release)));
    }

    #[test]
    fn soltar_una_letra_no_desarma_ni_dispara() {
        // Una letra que se tenía pulsada ANTES de Alt y se suelta después.
        let mut a = AltSolo::default();
        a.tecla(&alt(KeyEventKind::Press));
        assert!(!a.tecla(&ev(
            KeyCode::Char('a'),
            KeyEventKind::Release,
            KeyModifiers::NONE
        )));
        assert!(a.tecla(&alt(KeyEventKind::Release)));
    }

    #[test]
    fn las_modificadoras_no_son_teclas_para_el_keymap() {
        assert!(es_modificador(&alt(KeyEventKind::Press)));
        assert!(!es_modificador(&ev(
            KeyCode::Char('a'),
            KeyEventKind::Press,
            KeyModifiers::NONE
        )));
    }

    /// Encender escribe el PUSH con los cuatro flags y apagar el POP, una
    /// vez cada uno; y un terminal sin soporte no recibe nada. Un solo test
    /// porque el estado es de proceso.
    #[test]
    fn set_pide_y_retira_una_vez_y_respeta_el_soporte() {
        let mut out = Vec::new();
        set(false, || true, &mut out).expect("escribe");
        out.clear();

        set(true, || false, &mut out).expect("escribe");
        assert!(out.is_empty(), "sin soporte no se manda nada");

        set(true, || true, &mut out).expect("escribe");
        assert_eq!(String::from_utf8_lossy(&out), "\x1b[>15u");
        out.clear();
        set(true, || panic!("no vuelve a preguntar"), &mut out).expect("escribe");
        assert!(out.is_empty(), "idempotente");

        // Ceder y recuperar van EMPAREJADOS: dos veces cada uno es uno.
        ceder(&mut out).expect("escribe");
        ceder(&mut out).expect("escribe");
        recuperar(&mut out).expect("escribe");
        recuperar(&mut out).expect("escribe");
        assert_eq!(
            String::from_utf8_lossy(&out),
            "\x1b[<1u\x1b[>15u",
            "un pop y un push"
        );
        out.clear();

        set(false, || true, &mut out).expect("escribe");
        assert_eq!(String::from_utf8_lossy(&out), "\x1b[<1u");
        out.clear();
        ceder(&mut out).expect("escribe");
        assert!(out.is_empty(), "apagado no hay nada que ceder");

        // Apagar mientras está CEDIDO no quita nada: ya está fuera de la pila.
        set(true, || true, &mut out).expect("escribe");
        ceder(&mut out).expect("escribe");
        out.clear();
        set(false, || true, &mut out).expect("escribe");
        assert!(out.is_empty(), "cedido no se quita dos veces");
        recuperar(&mut out).expect("escribe");
        assert!(out.is_empty(), "apagado no se recupera");

        // Tras un pánico, la salida no vuelve a quitarlo.
        set(true, || true, &mut out).expect("escribe");
        out.clear();
        soltar_en_panico(&mut out).expect("escribe");
        set(false, || true, &mut out).expect("escribe");
        assert_eq!(String::from_utf8_lossy(&out), "\x1b[<1u", "un solo pop");
    }
}
