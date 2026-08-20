//! Qué comandos sabe ejecutar este host, y qué hace con los que no.
//!
//! La lista importa por dos motivos. Uno: es lo que el keymap efectivo
//! necesita para decidir si una tecla ligada puede ejecutarse AQUÍ
//! (`Availability::NotHere` es «este frontend no lo implementa», y sin la
//! lista no se puede distinguir de «norte no lo ha construido»). Y dos: es
//! la única declaración honesta de hasta dónde llega el host, en vez de un
//! `match` que se traga en silencio lo que no reconoce.

/// Los comandos que el host ejecuta HOY.
///
/// Crece con cada tarea de la fase 2. Todo lo demás del catálogo resuelve a
/// [`norte_frontend::keymap::Availability::NotHere`] y se DICE en la barra,
/// que es exactamente lo que hace el TUI con los suyos.
pub const IMPLEMENTADOS: &[&str] = &[
    "cursor.up",
    "cursor.down",
    "cursor.page-up",
    "cursor.page-down",
    "cursor.top",
    "cursor.bottom",
    "nav.enter",
    "nav.parent",
    "nav.back",
    "nav.forward",
    "mark.toggle",
    "mark.clear",
];

/// Lo que un comando le pide al hueco con el foco.
///
/// Es el vocabulario INTERNO del host: el renderer nunca lo ve. Existe para
/// que el resolver y el ratón acaben en el mismo sitio — un gesto y una
/// tecla que significan lo mismo tienen que hacer lo mismo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Efecto {
    /// Mueve el cursor tantas filas (negativo hacia arriba).
    Cursor(i64),
    /// Mueve el cursor tantas PÁGINAS (negativo hacia arriba). Cuántas filas
    /// son lo decide el hueco con la ventana que el renderer le dijo.
    Pagina(i64),
    /// Cursor al principio o al final del listado.
    Extremo {
        /// `true` = al final.
        al_final: bool,
    },
    /// Entra en lo que haya bajo el cursor.
    Entrar,
    /// Sube al directorio padre.
    Subir,
    /// Rastro de navegación.
    Rastro {
        /// `true` = atrás.
        atras: bool,
    },
    /// Marca o desmarca la fila del cursor.
    Marcar,
    /// Quita todas las marcas.
    DesmarcarTodo,
}

/// Traduce un comando del catálogo al efecto que el host aplica.
///
/// `None` = el host no lo implementa. No es un descarte silencioso: quien
/// llama lo convierte en un `Unavailable` que el usuario ve.
#[must_use]
pub fn efecto_de(command: &str, veces: u32) -> Option<Efecto> {
    let n = i64::from(veces.max(1).min(u32::from(u16::MAX)));
    Some(match command {
        "cursor.up" => Efecto::Cursor(-n),
        "cursor.down" => Efecto::Cursor(n),
        // Una página son las filas VISIBLES, y cuántas son lo sabe el hueco
        // (el renderer se lo dijo con `SetVisibleRange`): por eso viaja como
        // páginas y no como filas.
        "cursor.page-up" => Efecto::Pagina(-n),
        "cursor.page-down" => Efecto::Pagina(n),
        "cursor.top" => Efecto::Extremo { al_final: false },
        "cursor.bottom" => Efecto::Extremo { al_final: true },
        "nav.enter" => Efecto::Entrar,
        "nav.parent" => Efecto::Subir,
        "nav.back" => Efecto::Rastro { atras: true },
        "nav.forward" => Efecto::Rastro { atras: false },
        "mark.toggle" => Efecto::Marcar,
        "mark.clear" => Efecto::DesmarcarTodo,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Todo lo que se declara implementado tiene efecto, y al revés. Sin
    /// esto, la lista y el `match` se separan y `Availability` empieza a
    /// mentir.
    #[test]
    fn la_lista_y_los_efectos_no_pueden_separarse() {
        for c in IMPLEMENTADOS {
            assert!(
                efecto_de(c, 1).is_some(),
                "{c} está en la lista y no tiene efecto"
            );
        }
    }

    /// Y todo lo declarado existe en el catálogo compartido: un comando
    /// inventado aquí no lo ligaría ningún preset.
    #[test]
    fn todo_lo_declarado_esta_en_el_catalogo() {
        for c in IMPLEMENTADOS {
            assert!(
                norte_frontend::keymap::CATALOGUE
                    .iter()
                    .any(|d| d.name == *c),
                "{c} no está en el catálogo compartido"
            );
        }
    }

    /// El contador multiplica lo que se puede repetir.
    #[test]
    fn el_contador_multiplica() {
        assert_eq!(efecto_de("cursor.down", 3), Some(Efecto::Cursor(3)));
        assert_eq!(efecto_de("cursor.up", 3), Some(Efecto::Cursor(-3)));
        assert_eq!(efecto_de("cursor.page-down", 2), Some(Efecto::Pagina(2)));
    }
}
