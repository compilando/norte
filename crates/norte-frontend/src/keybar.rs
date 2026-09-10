//! La barra de teclas de función (spec 2026-09-10): diez celdas, `F1`–`F10`,
//! con lo que cada una hace en la pantalla actual.
//!
//! DERIVADA del keymap efectivo, nunca escrita a mano: reatar `F5` cambia su
//! etiqueta, y una pantalla que no ata `F7` deja la celda en blanco. Es la
//! seña de identidad del gestor ortodoxo y la ayuda de descubrimiento más
//! barata que hay; los dos frontends la pintan de aquí.

use crate::keymap::Effective;
use norte_i18n::{Lang, t_in};

/// Una celda de la barra.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyCell {
    /// `1`..=`10`.
    pub key: u8,
    /// La etiqueta corta, en el idioma pedido. Vacía = la tecla no ata nada
    /// en esta pantalla.
    pub label: String,
    /// El comando que corre, si alguno.
    pub command: Option<String>,
}

/// Cuántas celdas tiene la barra.
pub const CELLS: u8 = 10;

/// Las diez celdas de una pantalla, en el idioma dado.
///
/// La etiqueta es la del MENÚ (`menu-item-<cmd>`, que existe para todo
/// comando vivo), y si no la hay, la descripción de la ayuda
/// (`help-cmd-<cmd>`), y si tampoco, el último tramo del id: un comando de
/// plugin o uno que nadie tradujo sigue diciendo algo.
///
/// ```
/// use norte_frontend::keybar::cells_in;
/// use norte_frontend::keymap::{Effective, Screen, parse_keymap};
/// use norte_i18n::Lang;
///
/// let preset = parse_keymap(
///     "[pane]\nkeymap = [{ on = [\"f5\"], run = \"pane.copy\" }]\n",
/// )
/// .unwrap();
/// let eff = Effective::build_for(&preset, &[], &["pane.copy"], Screen::Browse).unwrap();
/// let cells = cells_in(&eff, Lang::En);
/// assert_eq!(cells.len(), 10);
/// assert_eq!(cells[4].command.as_deref(), Some("pane.copy"));
/// assert!(!cells[4].label.is_empty());
/// assert!(cells[0].label.is_empty() && cells[0].command.is_none());
/// ```
#[must_use]
pub fn cells_in(eff: &Effective, lang: Lang) -> Vec<KeyCell> {
    let bindings = eff.bindings();
    (1..=CELLS)
        .map(|n| {
            let chord = format!("f{n}");
            let command = bindings
                .iter()
                .find(|(seq, _)| *seq == chord)
                .map(|(_, cmd)| (*cmd).to_owned());
            let label = command
                .as_deref()
                .map(|c| label_in(c, lang))
                .unwrap_or_default();
            KeyCell {
                key: n,
                label,
                command,
            }
        })
        .collect()
}

/// La etiqueta corta de un comando, en el idioma dado.
fn label_in(command: &str, lang: Lang) -> String {
    let dashed = command.replace('.', "-");
    for prefix in ["menu-item-", "help-cmd-"] {
        let clave = format!("{prefix}{dashed}");
        let texto = t_in(lang, &clave);
        // El contrato de `t_in` es devolver la clave cuando falta.
        if texto != clave {
            return texto;
        }
    }
    command.rsplit('.').next().unwrap_or(command).to_owned()
}

/// Dónde cae cada celda en una fila de `width` celdas: `(x0, ancho)` por
/// tecla, en orden. La fila se reparte a partes iguales y el resto se lo
/// quedan las últimas, para que las diez existan siempre que haya diez
/// celdas; con menos, las que caben. Pintado y zonas del ratón salen de
/// aquí, así que miden lo mismo.
#[must_use]
pub fn layout(width: usize) -> Vec<(usize, usize)> {
    let n = usize::from(CELLS);
    if width < n {
        return (0..width).map(|x| (x, 1)).collect();
    }
    let base = width / n;
    let extra = width % n;
    let mut x = 0;
    (0..n)
        .map(|i| {
            let w = base + usize::from(i >= n - extra);
            let cell = (x, w);
            x += w;
            cell
        })
        .collect()
}

/// El texto de una celda que mide `width`: el número pegado a la
/// izquierda y la etiqueta detrás, recortada por celdas. `F` no se pinta:
/// diez celdas de `F` no dicen nada y cuestan diez columnas.
#[must_use]
pub fn cell_text(cell: &KeyCell, width: usize) -> String {
    let num = cell.key.to_string();
    let room = width.saturating_sub(num.len());
    let label = crate::middle_ellipsis(&cell.label, room);
    let pad = room.saturating_sub(crate::display::cells(&label));
    format!("{num}{label}{}", " ".repeat(pad))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La fila se reparte entera y en orden; con menos de diez celdas, las
    /// que caben; y el texto de una celda mide exactamente su ancho.
    #[test]
    fn el_reparto_cubre_la_fila_y_el_texto_mide_su_celda() {
        let l = layout(83);
        assert_eq!(l.len(), 10);
        assert_eq!(l[0], (0, 8));
        assert_eq!(l[9].0 + l[9].1, 83, "la última acaba en el borde");
        assert!(l.windows(2).all(|w| w[0].0 + w[0].1 == w[1].0));
        assert_eq!(layout(4).len(), 4);
        let c = KeyCell {
            key: 10,
            label: "Salir de norte ya".into(),
            command: Some("app.quit".into()),
        };
        assert_eq!(crate::display::cells(&cell_text(&c, 8)), 8);
        assert!(cell_text(&c, 8).starts_with("10"));
        let vacia = KeyCell {
            key: 7,
            label: String::new(),
            command: None,
        };
        assert_eq!(cell_text(&vacia, 6), "7     ");
    }

    /// Sin traducción de menú ni de ayuda, el último tramo del id.
    #[test]
    fn la_etiqueta_cae_al_id_cuando_nadie_lo_tradujo() {
        assert_eq!(
            label_in("plugin:acme:frobnicate", Lang::En),
            "plugin:acme:frobnicate"
        );
        assert_eq!(label_in("dialog.no-existe", Lang::Es), "no-existe");
    }
}
