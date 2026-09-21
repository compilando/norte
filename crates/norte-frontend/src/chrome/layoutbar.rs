//! Los botones de disposición, a la derecha de la barra de menús (ADR 0133).
//!
//! Los de VS Code arriba a la derecha dicen «aquí se cambia la forma de la
//! pantalla» sin abrir un menú. En norte son cuatro órdenes que ya existen
//! —partir lado a lado, partir arriba y abajo, igualar, elegir disposición—
//! y esta tabla es la ÚNICA lista: la TUI los pinta como celdas ASCII y la
//! ventana como iconos, y los dos pulsan por el mismo `id`.

use norte_i18n::{Lang, t_in};

/// Un botón de disposición.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutButton {
    /// Id estable: el que vuelve con un clic y el que elige el icono.
    pub id: &'static str,
    /// La orden del catálogo que corre.
    pub command: &'static str,
    /// Cómo se pinta en el terminal: ASCII entre corchetes, como `[+]` en
    /// la barra de pestañas — un símbolo de caja puede medir dos celdas.
    pub glyph: &'static str,
}

/// Los cinco, en el orden en que se pintan.
pub const BUTTONS: [LayoutButton; 5] = [
    LayoutButton {
        id: "split-h",
        command: "layout.split-h",
        glyph: "[|]",
    },
    LayoutButton {
        id: "split-v",
        command: "layout.split-v",
        glyph: "[-]",
    },
    LayoutButton {
        id: "equalize",
        command: "layout.equalize",
        glyph: "[=]",
    },
    // Girar (ADR 0138): lado a lado ↔ uno encima del otro.
    LayoutButton {
        id: "flip",
        command: "layout.flip",
        glyph: "[/]",
    },
    LayoutButton {
        id: "pick",
        command: "layout.pick",
        glyph: "[#]",
    },
];

/// El botón de un id, si existe.
#[must_use]
pub fn by_id(id: &str) -> Option<&'static LayoutButton> {
    BUTTONS.iter().find(|b| b.id == id)
}

/// Su nombre corto: el MISMO que su entrada del menú.
#[must_use]
pub fn label(b: &LayoutButton, lang: Lang) -> String {
    t_in(lang, &format!("menu-item-{}", b.command.replace('.', "-")))
}

/// Celdas que ocupan todos en el terminal, con un espacio entre dos.
#[must_use]
pub fn width() -> usize {
    width_of(&BUTTONS.iter().collect::<Vec<_>>())
}

/// Celdas que ocupan `botones` en el terminal, con un espacio entre dos.
#[must_use]
pub fn width_of(botones: &[&LayoutButton]) -> usize {
    botones.iter().map(|b| b.glyph.len()).sum::<usize>() + botones.len().saturating_sub(1)
}

/// El que cede primero cuando no caben todos: girar (ADR 0138) es el más
/// reciente y el que menos se usa, y los cuatro de siempre no pueden
/// desaparecer por su culpa en un terminal de cien columnas.
const PRESCINDIBLE: &str = "flip";

/// Los botones que caben en `disponible` celdas: todos, o todos menos
/// girar (el que cede primero), o ninguno. Nunca un conjunto a medias al azar: un
/// botón que está unas veces sí y otras no, según el ancho, se aprende
/// peor que uno que cede siempre el mismo.
#[must_use]
pub fn fitting(disponible: usize) -> Vec<&'static LayoutButton> {
    let todos: Vec<&LayoutButton> = BUTTONS.iter().collect();
    if width_of(&todos) <= disponible {
        return todos;
    }
    let sin: Vec<&LayoutButton> = BUTTONS.iter().filter(|b| b.id != PRESCINDIBLE).collect();
    if width_of(&sin) <= disponible {
        return sin;
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cada botón corre una orden que existe y tiene nombre en los dos
    /// idiomas: un botón mudo o que no hace nada es peor que no tenerlo.
    #[test]
    fn cada_boton_existe_y_se_nombra() {
        for b in &BUTTONS {
            assert!(
                crate::keymap::catalogue::lookup(b.command).is_some(),
                "{}",
                b.command
            );
            for lang in [Lang::Es, Lang::En] {
                assert!(
                    !label(b, lang).starts_with("menu-item-"),
                    "{} {lang:?}",
                    b.id
                );
            }
            assert!(b.glyph.is_ascii());
            assert_eq!(by_id(b.id), Some(b));
        }
        assert_eq!(width(), 19);
    }

    /// Sin sitio para los cinco, cede girar; sin sitio para cuatro, ninguno.
    #[test]
    fn cede_primero_girar() {
        assert_eq!(fitting(19).len(), 5);
        let cuatro = fitting(18);
        assert_eq!(cuatro.len(), 4);
        assert!(cuatro.iter().all(|b| b.id != "flip"));
        assert_eq!(width_of(&cuatro), 15);
        assert!(fitting(14).is_empty());
    }
}
