//! El panel de terminal en la ventana (#362, puente 95).
//!
//! Aquí sólo está la TRADUCCIÓN: la rejilla que `norte-term` mantiene, pasada
//! a la vista que cruza el puente. La emulación es la misma que usa la
//! terminal —el mismo crate, el mismo código—, y eso es lo que hace que los
//! dos frontends enseñen lo mismo por construcción, no porque alguien compare
//! dos emuladores que pueden divergir.
//!
//! # Lo que NO se hace aquí, y es la decisión
//!
//! **Un color indexado no se resuelve.** El shell dice «color 4»; qué azul es
//! eso lo decide la paleta de quien pinta. Si se resolviera aquí a un
//! `#rrggbb`, el panel dejaría de obedecer al tema del lector y no habría
//! forma de arreglarlo desde el tema. Por eso [`TerminalColorView`] conserva
//! los dos casos distintos.
//!
//! **No se enmascara nada.** Lo que sale de la rejilla ya no puede llevar un
//! byte de control: el parser se come los escapes y tira los C0 que no mueven
//! el cursor. El enmascarado de nombres hostiles existe porque un nombre llega
//! crudo; esto no llega crudo, llega parseado.

use norte_term::{ColorTerm, Estilo, Pantalla};

use crate::dto::{TerminalColorView, TerminalSlotView, TerminalSpanView};

/// La rejilla de un shell, pasada a la vista del puente.
///
/// `con_teclado` decide si viaja el cursor: uno parpadeando en un panel que no
/// tiene el teclado dice que el teclado está ahí, y no lo está.
#[must_use]
pub fn vista(slot_id: u32, pantalla: Option<&Pantalla>, con_teclado: bool) -> TerminalSlotView {
    let Some(p) = pantalla else {
        return TerminalSlotView {
            slot_id,
            rows: Vec::new(),
            cursor: None,
            no_shell: true,
        };
    };
    let (_, alto) = p.tamano();
    TerminalSlotView {
        slot_id,
        rows: (0..alto)
            .map(|f| {
                p.fila_tramos(f)
                    .into_iter()
                    .map(|(text, estilo)| span(text, estilo))
                    .collect()
            })
            .collect(),
        cursor: cursor(p, con_teclado),
        no_shell: false,
    }
}

/// Dónde va el cursor, o `None` si no se pinta.
fn cursor(p: &Pantalla, con_teclado: bool) -> Option<(u16, u16)> {
    if !con_teclado || !p.cursor_visible() {
        return None;
    }
    let (fila, col) = p.cursor();
    let (ancho, alto) = p.tamano();
    // La columna puede valer tanto como el ancho —el estado «pendiente de
    // salto»— y ahí el cursor se pinta en la última celda, que es donde lo
    // deja un terminal de verdad.
    (fila < alto).then(|| (fila, col.min(ancho.saturating_sub(1))))
}

fn span(text: String, e: Estilo) -> TerminalSpanView {
    TerminalSpanView {
        text,
        fg: color(e.fg),
        bg: color(e.bg),
        bold: e.negrita,
        dim: e.tenue,
        italic: e.cursiva,
        underline: e.subrayado,
        reverse: e.inverso,
        strike: e.tachado,
    }
}

fn color(c: ColorTerm) -> Option<TerminalColorView> {
    match c {
        ColorTerm::PorDefecto => None,
        ColorTerm::Indexado(index) => Some(TerminalColorView::Indexed { index }),
        ColorTerm::Rgb(r, g, b) => Some(TerminalColorView::Rgb {
            hex: format!("#{r:02x}{g:02x}{b:02x}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un índice cruza el puente SIN resolver, y un RGB como hex.
    ///
    /// Es la decisión del módulo, y se fija con un test porque el día que
    /// alguien «mejore» esto resolviendo el índice contra el tema, el panel
    /// dejará de obedecer al tema del lector y nada más se pondrá rojo.
    #[test]
    fn un_indice_sigue_siendo_un_indice_y_un_rgb_es_hex() {
        let mut p = Pantalla::nueva(12, 1);
        p.alimentar(b"\x1b[31ma\x1b[38;2;1;2;3mb");
        let v = vista(7, Some(&p), false);
        let fila = &v.rows[0];
        assert_eq!(
            fila[0].fg,
            Some(TerminalColorView::Indexed { index: 1 }),
            "el color 1 del shell no se resuelve aquí"
        );
        assert_eq!(
            fila[1].fg,
            Some(TerminalColorView::Rgb {
                hex: "#010203".to_owned()
            })
        );
    }

    /// Sin shell, la vista lo DICE en vez de mandar una rejilla vacía: un
    /// panel en blanco y uno sin shell se ven igual y no son lo mismo.
    #[test]
    fn sin_shell_se_dice() {
        let v = vista(3, None, true);
        assert!(v.no_shell);
        assert!(v.rows.is_empty());
        assert_eq!(v.cursor, None);
    }

    /// El cursor sólo viaja con el teclado dentro, y nunca si el shell lo
    /// escondió — que es lo que hace cualquier programa de pantalla completa.
    #[test]
    fn el_cursor_viaja_solo_cuando_se_pinta() {
        let mut p = Pantalla::nueva(10, 3);
        p.alimentar(b"hola");
        assert_eq!(vista(1, Some(&p), false).cursor, None, "sin teclado");
        assert_eq!(vista(1, Some(&p), true).cursor, Some((0, 4)));
        p.alimentar(b"\x1b[?25l");
        assert_eq!(vista(1, Some(&p), true).cursor, None, "escondido");
    }

    /// Las filas van TODAS las que tiene la rejilla, incluidas las vacías: un
    /// terminal no se desplaza como una lista, se repinta, y un renderer que
    /// recibiera sólo las escritas tendría que adivinar el alto.
    #[test]
    fn van_todas_las_filas() {
        let mut p = Pantalla::nueva(6, 4);
        p.alimentar(b"una");
        let v = vista(1, Some(&p), true);
        assert_eq!(v.rows.len(), 4);
    }
}
