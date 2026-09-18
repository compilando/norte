//! El RELEVO a la ventana (fase 9 del programa WOW): qué se lanza y cómo.
//!
//! Lo de AQUÍ es sólo la mitad que este proceso puede decidir sin hablar con
//! nadie: qué binario es la ventana y con qué argumentos arranca. Volcar la
//! pantalla y soltar la sesión es del escritor de sesión
//! ([`crate::session_push::request_handoff`]), y lanzarlo, del bucle.

use std::ffi::OsString;

/// Los nombres bajo los que la ventana puede estar instalada, en orden de
/// preferencia.
///
/// Dos y no uno porque `just link-gui` deja los dos: `ntc-gui` es el nombre
/// del binario y `norte-gui` el alias que la gente escribe. Buscarlos por
/// PATH —y no una ruta compilada— es lo que hace que un norte instalado de
/// cualquiera de las tres maneras (paquete, `cargo install`, symlink al árbol
/// de desarrollo) releve al que el lector tiene de verdad.
const VENTANA: &[&str] = &["ntc-gui", "norte-gui"];

/// El `argv` con el que arranca la ventana de un relevo.
///
/// `--attach` es lo que la distingue de un arranque cualquiera: además de la
/// pantalla, reclama lo MARCADO que este proceso acaba de dejar en la sesión.
/// Sin él la ventana abriría donde estabas pero sin lo que tenías señalado,
/// que es justo la mitad que no se puede rehacer con un `cd`.
///
/// `--daemon` viaja si este proceso lo lleva, y tiene que viajar: el relevo
/// sólo existe en modo daemon —es el daemon quien tiene la sesión— y una
/// ventana que arrancase contra su core embebido no encontraría nada de lo
/// que se acaba de soltar.
#[must_use]
pub fn window_argv(daemon: bool) -> Vec<OsString> {
    let mut argv = vec![OsString::from(programa())];
    argv.push(OsString::from("--attach"));
    if daemon {
        argv.push(OsString::from("--daemon"));
    }
    argv
}

/// El primero de [`VENTANA`] que está en el PATH, o el primero a secas.
///
/// Devolver el primero cuando no hay ninguno no es fingir que existe: el
/// lanzamiento falla, el bucle lo dice y el lector se queda donde estaba. La
/// alternativa —negarse aquí— convertiría «no tienes la ventana instalada» en
/// un `Option` que el llamante tendría que explicar dos veces.
fn programa() -> &'static str {
    VENTANA
        .iter()
        .copied()
        .find(|p| norte_frontend::openers::program_available(std::ffi::OsStr::new(p)))
        .unwrap_or(VENTANA[0])
}

/// Lanza `argv` SUELTO: este proceso se va detrás, así que la ventana no puede
/// quedarse colgando de él.
///
/// Ni `stdin` ni `stdout` ni `stderr` se heredan. La terminal es de la TUI, y
/// una ventana que escriba en ella después de que el relevo la devuelva al
/// shell ensucia el prompt del lector con trazas que no pidió.
///
/// # Errors
/// Lo que dé el `spawn`: que el binario no esté es el caso normal, y se dice.
pub fn spawn_window(argv: &[OsString]) -> std::io::Result<()> {
    let (programa, resto) = argv
        .split_first()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "argv vacío"))?;
    std::process::Command::new(programa)
        .args(resto)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--attach` va SIEMPRE: es lo que separa un relevo de un arranque, y sin
    /// él la ventana abre donde estabas pero sin lo que tenías marcado.
    #[test]
    fn el_relevo_siempre_pide_attach() {
        let argv = window_argv(false);
        assert!(argv.iter().any(|a| a == "--attach"), "{argv:?}");
        assert!(
            !argv.iter().any(|a| a == "--daemon"),
            "sin daemon no se inventa: {argv:?}"
        );
    }

    /// Y `--daemon` viaja si este proceso lo lleva: la sesión que se acaba de
    /// soltar es la del daemon, y una ventana contra su core embebido no
    /// encontraría nada.
    #[test]
    fn el_daemon_viaja_con_el_relevo() {
        let argv = window_argv(true);
        assert!(argv.iter().any(|a| a == "--daemon"), "{argv:?}");
    }

    /// Un `argv` vacío no llega a `spawn`: se rechaza con un error en vez de
    /// indexar.
    #[test]
    fn un_argv_vacio_no_revienta() {
        assert!(spawn_window(&[]).is_err());
    }
}
