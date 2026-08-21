//! La entrada de teclado, y por qué el renderer no la interpreta.
//!
//! Un renderer manda TECLAS —normalizadas, y poco más—; quién resuelve un
//! contador, un prefijo a medias o qué comando es `ctrl+shift+f5` es Rust,
//! con el mismo resolver, los mismos presets y el mismo catálogo que usa el
//! TUI (ADR 0066, decisión D14). Si el renderer resolviera, habría dos
//! keymaps y el día que divergieran nadie lo notaría hasta que un usuario lo
//! contase.
//!
//! # El adaptador es delgado a propósito
//!
//! Lo único que hay aquí es la traducción del vocabulario del renderer
//! (`"ArrowDown"`, `"Escape"`, `meta`) al del proyecto (`down`, `esc`,
//! `mod`). El mapeo de `mod` en macOS es entrada del adaptador, NO un keymap
//! bifurcado: el chord que sale de aquí es el mismo tipo que el TUI empuja a
//! su resolver.

use norte_frontend::keymap::{Chord, KeymapError, parse_chord};
use serde::{Deserialize, Serialize};

/// Una tecla tal como la manda el renderer.
///
/// Cuatro banderas y no un conjunto de modificadores: es la forma en la que
/// un navegador —y cualquier toolkit— entrega el evento, y traducir en el
/// borde es más barato que obligar a cada adaptador a construir un tipo
/// nuestro. El chord que sale de aquí ya es el del keymap.
#[allow(clippy::struct_excessive_bools)] // la forma del evento de entrada, no un estado
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyInput {
    /// Nombre LÓGICO de la tecla. Se aceptan los del navegador
    /// (`"ArrowDown"`, `"Escape"`, `"F5"`, `"a"`) y los del proyecto
    /// (`"down"`, `"esc"`).
    pub key: String,
    /// Control.
    #[serde(default)]
    pub ctrl: bool,
    /// Alt / Option.
    #[serde(default)]
    pub alt: bool,
    /// Mayúsculas.
    #[serde(default)]
    pub shift: bool,
    /// Command (macOS) o Super. Viaja como `mod`, que es lo que el keymap
    /// entiende y lo que hace que un preset valga en las dos plataformas.
    #[serde(default)]
    pub meta: bool,
}

impl KeyInput {
    /// Traduce a un [`Chord`] del keymap compartido.
    ///
    /// ```
    /// use norte_ui_host::KeyInput;
    ///
    /// let k = KeyInput {
    ///     key: "ArrowDown".to_owned(),
    ///     ctrl: false,
    ///     alt: false,
    ///     shift: false,
    ///     meta: false,
    /// };
    /// assert!(k.to_chord().is_ok());
    ///
    /// // Una tecla que no se entiende se descarta; no se adivina.
    /// let rara = KeyInput { key: "Compose".to_owned(), ..k };
    /// assert!(rara.to_chord().is_err());
    /// ```
    ///
    /// # Errors
    /// [`KeymapError::BadChord`] si el nombre de tecla no se reconoce: una
    /// tecla que no se entiende se DESCARTA, jamás se adivina.
    pub fn to_chord(&self) -> Result<Chord, KeymapError> {
        let nombre = nombre_canonico(&self.key).ok_or_else(|| KeymapError::BadChord {
            chord: self.key.clone(),
        })?;
        let mut texto = String::new();
        // El ORDEN de los modificadores es el que el parser espera; el
        // renderer no tiene por qué saberlo.
        if self.ctrl {
            texto.push_str("ctrl+");
        }
        if self.alt {
            texto.push_str("alt+");
        }
        if self.shift {
            texto.push_str("shift+");
        }
        if self.meta {
            texto.push_str("mod+");
        }
        texto.push_str(&nombre);
        parse_chord(&texto)
    }
}

/// El nombre de tecla del proyecto para lo que mande el renderer.
///
/// Acepta las dos ortografías —la del navegador y la nuestra— porque el
/// adaptador de cada renderer no tiene por qué normalizar dos veces, y
/// porque un renderer que ya manda `"down"` no debería ser el caso raro.
fn nombre_canonico(key: &str) -> Option<String> {
    let bajo = key.to_ascii_lowercase();
    let canonico = match bajo.as_str() {
        "arrowdown" | "down" => "down",
        "arrowup" | "up" => "up",
        "arrowleft" | "left" => "left",
        "arrowright" | "right" => "right",
        "escape" | "esc" => "esc",
        "enter" | "return" => "enter",
        "tab" => "tab",
        "backspace" => "backspace",
        "delete" | "del" => "delete",
        "insert" | "ins" => "insert",
        "home" => "home",
        "end" => "end",
        "pageup" | "pgup" => "pageup",
        "pagedown" | "pgdn" => "pagedown",
        " " | "space" | "spacebar" => "space",
        otro => {
            // Teclas de función y caracteres sueltos. Un nombre largo que no
            // esté en la tabla NO se interpreta como texto: sería la puerta
            // por la que `"F13"` acaba siendo tres caracteres.
            if let Some(n) = otro.strip_prefix('f')
                && !n.is_empty()
                && n.chars().all(|c| c.is_ascii_digit())
            {
                return Some(otro.to_owned());
            }
            if otro.chars().count() == 1 {
                // Con Mayús, la letra la manda el renderer ya en mayúscula
                // (es lo que el usuario ve); el keymap la quiere tal cual.
                return Some(key.to_owned());
            }
            return None;
        }
    };
    Some(canonico.to_owned())
}

/// El keymap efectivo de un preset de fábrica, con la lista de comandos que
/// este host implementa.
///
/// Lo mínimo para arrancar sin configuración —un test, un primer arranque—.
/// Un host de verdad fusiona además las capas del usuario y le pasa el
/// resultado en [`crate::UiHostOptions`]: leer configuración no es asunto de
/// este crate.
///
/// # Errors
/// [`KeymapError`] si el preset no existe o no valida.
pub fn keymap_de_preset(nombre: &str) -> Result<norte_frontend::keymap::Effective, KeymapError> {
    keymap_de_preset_con(nombre, crate::commands::Efectos::Completo)
}

/// El keymap del listado para un frontend con los efectos DICHOS.
///
/// En solo lectura, los comandos que escriben no entran en la lista de
/// conocidos, así que una tecla atada a `pane.delete` resuelve a
/// [`norte_frontend::keymap::Availability::NotHere`] y se dice — que es lo
/// que un usuario necesita leer, en vez de una tecla muda.
///
/// # Errors
/// [`KeymapError`] si el preset no existe o no valida.
pub fn keymap_de_preset_con(
    nombre: &str,
    efectos: crate::commands::Efectos,
) -> Result<norte_frontend::keymap::Effective, KeymapError> {
    efectivo_con(nombre, norte_frontend::keymap::Screen::Browse, efectos)
}

/// El keymap efectivo de la pantalla del VISOR, del mismo preset.
///
/// Es OTRA pantalla, no otra capa: con el visor abierto las teclas son suyas
/// —`esc` cierra, `e` cambia el encoding— y mezclarlas con las del listado
/// sería un contexto de entrada que no existe en ningún preset.
///
/// # Errors
/// [`KeymapError`] si el preset no existe o no valida.
pub fn keymap_visor_de_preset(
    nombre: &str,
) -> Result<norte_frontend::keymap::Effective, KeymapError> {
    efectivo(nombre, norte_frontend::keymap::Screen::Viewer)
}

fn efectivo(
    nombre: &str,
    pantalla: norte_frontend::keymap::Screen,
) -> Result<norte_frontend::keymap::Effective, KeymapError> {
    efectivo_con(nombre, pantalla, crate::commands::Efectos::Completo)
}

fn efectivo_con(
    nombre: &str,
    pantalla: norte_frontend::keymap::Screen,
    efectos: crate::commands::Efectos,
) -> Result<norte_frontend::keymap::Effective, KeymapError> {
    let fuente = norte_frontend::keymap::presets::source(nombre).ok_or(KeymapError::BadChord {
        chord: nombre.to_owned(),
    })?;
    let preset = norte_frontend::keymap::parse_keymap(fuente)?;
    norte_frontend::keymap::Effective::build_for(
        &preset,
        &[],
        &crate::commands::todos_con(efectos),
        pantalla,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_frontend::keymap::{KeyCode, Mods};

    #[test]
    fn el_vocabulario_del_navegador_se_traduce() {
        let k = KeyInput {
            key: "ArrowDown".to_owned(),
            ctrl: false,
            alt: false,
            shift: false,
            meta: false,
        };
        assert_eq!(
            k.to_chord().expect("chord"),
            Chord::new(Mods::default(), KeyCode::Down)
        );
    }

    #[test]
    fn los_modificadores_van_en_el_orden_del_parser() {
        let k = KeyInput {
            key: "F5".to_owned(),
            ctrl: true,
            alt: false,
            shift: true,
            meta: false,
        };
        let c = k.to_chord().expect("chord");
        assert_eq!(c, parse_chord("ctrl+shift+f5").expect("parse"));
    }

    /// `meta` viaja como `mod`: es el adaptador quien conoce macOS, no el
    /// keymap.
    #[test]
    fn meta_viaja_como_mod() {
        let k = KeyInput {
            key: "p".to_owned(),
            ctrl: false,
            alt: false,
            shift: false,
            meta: true,
        };
        assert_eq!(
            k.to_chord().expect("chord"),
            parse_chord("mod+p").expect("parse")
        );
    }

    /// Una tecla que no se reconoce se descarta; no se adivina.
    #[test]
    fn una_tecla_desconocida_no_se_inventa() {
        let k = KeyInput {
            key: "Compose".to_owned(),
            ctrl: false,
            alt: false,
            shift: false,
            meta: false,
        };
        assert!(k.to_chord().is_err());
    }
}
