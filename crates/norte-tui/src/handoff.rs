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
/// Los argumentos salen de [`norte_frontend::handoff::window_args`], el
/// MISMO sitio del que la ventana saca su test de que los acepta. Antes se
/// escribían aquí a mano y llevaban `--daemon`, que la ventana no tiene
/// —siempre va con daemon—: salía con código 2 y, con `stderr` cerrado,
/// sin decir nada. El relevo quedaba en una terminal que se cerraba y una
/// ventana que no llegaba.
#[must_use]
pub fn window_argv() -> Vec<OsString> {
    let mut argv = vec![OsString::from(programa())];
    argv.extend(
        norte_frontend::handoff::window_args()
            .into_iter()
            .map(OsString::from),
    );
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
/// Devuelve el `Child` para que quien llama compruebe con [`esperar_arranque`]
/// que la ventana SIGUE viva: un `spawn` correcto sólo dice que el proceso
/// empezó.
///
/// # Errors
/// Lo que dé el `spawn`: que el binario no esté es el caso normal, y se dice.
pub fn spawn_window(argv: &[OsString]) -> std::io::Result<std::process::Child> {
    let (programa, resto) = argv
        .split_first()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "argv vacío"))?;
    std::process::Command::new(programa)
        .args(resto)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
}

/// Cuánto se espera a que la ventana demuestre que va a vivir.
///
/// Lo que se quiere cazar es la muerte INMEDIATA —un flag desconocido, una
/// librería que falta, un daemon que no arranca—, que ocurre en decenas de
/// milisegundos. Un segundo y medio sobra para eso y no se nota al entregar la
/// pantalla, que es un gesto de una vez; lo que no caza —una ventana que muere
/// a los diez segundos— tampoco lo cazaría esperar tres.
pub const GRACIA: std::time::Duration = std::time::Duration::from_millis(1_500);

/// ¿Sigue viva la ventana pasado `gracia`? `Err` con su código de salida si
/// murió antes (`None` si la mató una señal).
///
/// Es la mitad del relevo que faltaba: sin esto, la terminal se iba en cuanto
/// `spawn` decía `Ok`, y una ventana que salía con código 2 dejaba al lector
/// sin ninguna de las dos — lo contrario de lo que promete la ADR 0123.
///
/// Sondea con `try_wait`, que NO bloquea, y duerme con el reloj de tokio: el
/// bucle de eventos no se queda parado en una llamada al sistema (regla 2).
/// El `Child` que se suelta al volver no mata al proceso: `std` no lo hace al
/// dropearlo, y la ventana sigue su vida.
///
/// # Errors
/// El código de salida de una ventana que murió dentro de `gracia`.
pub async fn esperar_arranque(
    mut hijo: std::process::Child,
    gracia: std::time::Duration,
) -> Result<(), Option<i32>> {
    const PASO: std::time::Duration = std::time::Duration::from_millis(100);
    let limite = tokio::time::Instant::now() + gracia;
    loop {
        match hijo.try_wait() {
            Ok(Some(estado)) => return Err(estado.code()),
            // Sin poder preguntar no se sabe si murió, y la respuesta que no
            // deja al lector sin nada es darla por viva: la sesión ya está
            // escrita y suelta, y la ventana la reclamará si arranca.
            Err(_) => return Ok(()),
            Ok(None) => {}
        }
        if tokio::time::Instant::now() >= limite {
            return Ok(());
        }
        tokio::time::sleep(PASO).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--attach` va SIEMPRE, y `--daemon` NUNCA: la ventana no lo tiene, y un
    /// flag desconocido la mataba sin decir nada. Que la ventana acepta este
    /// `argv` lo prueba su propio parser, en su crate.
    #[test]
    fn a_la_ventana_se_le_pide_attach_y_nada_que_no_conozca() {
        let argv = window_argv();
        assert!(argv.iter().any(|a| a == "--attach"), "{argv:?}");
        assert!(!argv.iter().any(|a| a == "--daemon"), "{argv:?}");
    }

    /// Un `argv` vacío no llega a `spawn`: se rechaza con un error en vez de
    /// indexar.
    #[test]
    fn un_argv_vacio_no_revienta() {
        assert!(spawn_window(&[]).is_err());
    }

    /// Una ventana que MUERE nada más nacer no cuenta como relevo hecho.
    ///
    /// El bug que lo pidió: la ventana salía con código 2 por un flag que no
    /// conocía, y la terminal ya se había ido porque `spawn` había dicho `Ok`
    /// — que sólo significa que el proceso EMPEZÓ. El lector se quedaba sin
    /// ninguna de las dos, que es exactamente lo que la ADR 0123 promete que
    /// no pasa.
    #[tokio::test]
    async fn una_ventana_que_muere_al_nacer_no_es_un_relevo() {
        let hijo = std::process::Command::new("sh")
            .args(["-c", "exit 2"])
            .spawn()
            .expect("sh existe");
        assert_eq!(
            esperar_arranque(hijo, std::time::Duration::from_millis(1_500)).await,
            Err(Some(2)),
            "y dice con qué código salió"
        );
    }

    /// Y una que sigue viva pasado el rato de gracia, sí.
    #[tokio::test]
    async fn una_ventana_que_sigue_viva_es_un_relevo() {
        let mut hijo = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .expect("sleep existe");
        let pid = hijo.id();
        // El `Child` se lo queda la espera; para no dejar un `sleep` suelto,
        // se mata después por su pid.
        let _ = &mut hijo;
        assert_eq!(
            esperar_arranque(hijo, std::time::Duration::from_millis(300)).await,
            Ok(())
        );
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
    }
}
