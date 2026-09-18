//! Los argumentos del RELEVO entre frontends (fase 9, ADR 0123), en UN sitio.
//!
//! Existe por un bug que sólo se vio con un humano delante: la terminal
//! lanzaba la ventana como `ntc-gui --attach --daemon`, y la ventana no conocía
//! ninguno de los dos flags. Su parser rechaza lo desconocido —bien: un flag
//! mal escrito que se ignora es una opción que el usuario cree haber puesto—,
//! salía con código 2, y como el relevo le cierra `stderr` para no ensuciar el
//! prompt, moría sin decir nada. La terminal ya se había ido.
//!
//! El `argv` se construía en un binario y se parseaba en OTRO, sin nada que los
//! atara. Ahora los dos lados lo sacan de aquí, y cada binario tiene un test
//! que parsea lo que construye el otro con SU parser de verdad: es lo único que
//! habría visto el fallo sin abrir una ventana.

/// El flag que distingue un relevo de un arranque cualquiera.
///
/// Con él, el frontend que llega reclama además lo MARCADO que el otro dejó en
/// la sesión. Sin él, un arranque es un arranque, y unas marcas de un relevo a
/// medias no resucitan al día siguiente.
pub const ATTACH: &str = "--attach";

/// Los argumentos con los que la terminal lanza la VENTANA.
///
/// Sólo [`ATTACH`], y la ausencia es la mitad del arreglo: la ventana va
/// SIEMPRE con daemon —arranca el suyo si no hay—, así que no tiene `--daemon`,
/// y pasárselo era un flag desconocido que la mataba.
#[must_use]
pub fn window_args() -> Vec<String> {
    vec![ATTACH.to_owned()]
}

/// Los argumentos con los que la ventana lanza la TERMINAL (`ntc`).
///
/// [`ATTACH`] y `--daemon` si quien releva lo lleva: la sesión que se acaba de
/// soltar es la del daemon, y un `ntc` contra su core embebido no encontraría
/// nada. La ventana pasa `true` siempre, porque es su único modo.
#[must_use]
pub fn terminal_args(daemon: bool) -> Vec<String> {
    let mut args = vec![ATTACH.to_owned()];
    if daemon {
        args.push("--daemon".to_owned());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La ventana NO recibe `--daemon`: no lo tiene, y un flag desconocido la
    /// mataba sin decir nada. El test de que lo acepta vive en su crate, con su
    /// parser de verdad; éste fija que no se le vuelva a colar.
    #[test]
    fn a_la_ventana_solo_se_le_pide_attach() {
        assert_eq!(window_args(), vec!["--attach".to_owned()]);
    }

    /// A la terminal sí: sin `--daemon` iría contra su core embebido.
    #[test]
    fn a_la_terminal_se_le_pasa_el_daemon() {
        assert_eq!(
            terminal_args(true),
            vec!["--attach".to_owned(), "--daemon".to_owned()]
        );
        assert_eq!(terminal_args(false), vec!["--attach".to_owned()]);
    }
}
