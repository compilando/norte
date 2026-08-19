//! Ceder la terminal a otro programa y recuperarla intacta.
//!
//! Vivía en el root del binario `ntc` —un crate DISTINTO de esta lib—, así que
//! `suspension_outcome`, la única parte de la suspensión que se puede probar sin
//! una terminal de verdad, tenía sus tests dentro del binario.
//!
//! El invariante que ordena el módulo entero: [`suspend_terminal`] y
//! [`resume_terminal`] están EMPAREJADAS y ningún camino sale entre medias. La
//! review de S4 encontró lo contrario —una frontera puesta después de tres `?`
//! que ya habían tocado la terminal— y el síntoma era una TUI que seguía
//! pintando con el raw mode apagado.
//!
//! Este módulo no toca el `App`: la suspensión es un asunto entre la terminal y
//! un proceso hijo.

use norte_i18n::t;

use crate::mouse;
use crate::tty;

/// Suspende el TUI (sale de la pantalla alternativa + raw mode), corre `argv`
/// con la TERMINAL DE CONTROL como stdio en `cwd` y restaura en TODOS los
/// caminos.
///
/// Nació como `run_opener` (#28) y S4 (#135) la generalizó. La estructura de
/// restauración es la misma idea, con una diferencia que la review de S4
/// señaló (MAJOR-3): la frontera «a partir de aquí hay que restaurar» estaba
/// DESPUÉS de tres `?` que ya habían tocado la terminal, así que un
/// `LeaveAlternateScreen` fallido devolvía `Err` con el raw mode apagado y la
/// pantalla alternativa puesta — y el run loop seguía pintando una TUI cuyas
/// teclas ya no respondían y cuyo texto se hacía eco en el scrollback. Ahora
/// [`suspend_terminal`] y [`resume_terminal`] están emparejadas y NINGÚN
/// camino sale entre medias.
///
/// - `argv` VACÍO no lanza nada y devuelve `Ok(None)`: eso es
///   `app.toggle-panels`, que solo enseña la terminal anfitriona.
/// - `cwd` `None` deja el directorio de norte, que es lo que hacen hoy los
///   openers de #28 (su `%d` ya viaja DENTRO del argv, así que cambiárselo
///   aquí sería un cambio de comportamiento con la excusa de una refactor).
/// - `wait_for_key` retiene la terminal anfitriona a la vista hasta que el
///   usuario pulse algo. Sin ello el listado vuelve encima de la salida del
///   comando y no hay forma de leerla.
///
/// # El stdio del hijo es `/dev/tty`, no el heredado
///
/// Desde la tarea 1 la TUI pinta en la terminal de control PRECISAMENTE para
/// que stdout pueda llevar datos, y desde la 2 los lleva (`--pick` escribe
/// las rutas elegidas, terminadas en NUL). Un hijo con stdio heredado los
/// mezclaría con los suyos: `ntc --pick | xargs -0 …` seguido de F9 mete la
/// sesión entera del shell en la tubería, y el primer «path» que lee la
/// herramienta de abajo es la salida del shell pegada a la primera ruta —
/// abrir el fichero equivocado, no un defecto cosmético (review de S4, H1 y
/// MAJOR-1). Heredar stdin es igual de malo al revés: `ntc < /dev/null` daba
/// un F9 cuyo shell leía EOF y salía al instante, y parecía la tecla rota.
///
/// Si la terminal de control no se puede abrir se hereda, como antes: es
/// degradación, no un motivo para no lanzar nada.
///
/// # Ctrl+C mata al hijo, no a norte
///
/// Salir del raw mode devuelve `ISIG`, y el hijo se queda en el grupo de
/// procesos del primer plano junto con norte: sin manejador, el Ctrl+C con el
/// que se aborta un `make` mataría al gestor de ficheros entero (review de
/// S4, B1). Registrar SIGINT/SIGQUIT en tokio instala un manejador de proceso
/// —permanente, y eso está bien: en modo TUI el raw mode ya impide que esas
/// señales se generen— así que norte sobrevive y el hijo, cuyas disposiciones
/// `exec` devolvió a `SIG_DFL`, muere. SIGTSTP (Ctrl+Z) NO se cubre: suspender
/// norte con la terminal a medio ceder es un problema distinto, y está dicho
/// en los límites honestos del tema de ayuda.
///
/// El hijo hereda [`norte_frontend::shell::LEVEL_VAR`] incrementado. norte no
/// lo vuelve a leer nunca: el consumidor es el prompt del propio usuario, que
/// es donde hace falta saber que este shell salió de un norte.
/// # Errors
///
/// Lo que falle al ceder la terminal, al lanzar el hijo, al esperar la tecla
/// o al recuperarla — y en ese orden de precedencia, que es el que fija
/// [`suspension_outcome`]. Ceder la terminal es el único de los cuatro que
/// aborta la suspensión: los otros tres ya han pasado por la restauración
/// cuando se devuelven.
pub async fn run_suspended(
    terminal: &mut tty::Tui,
    capture: &mut mouse::Capture,
    argv: Vec<std::ffi::OsString>,
    cwd: Option<std::path::PathBuf>,
    wait_for_key: bool,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    // Registrado ANTES de ceder la terminal, y vivo hasta el final: ver «Ctrl+C
    // mata al hijo» arriba. Un fallo al registrar no impide suspender —
    // significa volver al comportamiento de antes, no quedarse sin la tecla.
    #[cfg(unix)]
    let _senales = {
        use tokio::signal::unix::{SignalKind, signal};
        (
            signal(SignalKind::interrupt()).ok(),
            signal(SignalKind::quit()).ok(),
        )
    };
    // El estado de la captura se lee ANTES de tocar nada: si la propia
    // liberación falla a mitad, la restauración tiene que saber a qué volver
    // (review de S4, MINOR-6).
    let raton = capture.active();
    // ---- frontera: de aquí en adelante, todo camino pasa por `resume_terminal`.
    let cedida = suspend_terminal(terminal, capture);
    if let Err(e) = cedida {
        let _ = resume_terminal(terminal, capture, raton);
        return Err(e);
    }
    let child = if argv.is_empty() {
        Ok(Ok(None))
    } else {
        let level = norte_frontend::shell::next_norte_level();
        let stdio = || {
            tty::open_controlling_terminal()
                .and_then(|f| f.try_clone())
                .map_or_else(
                    |_| std::process::Stdio::inherit(),
                    std::process::Stdio::from,
                )
        };
        tokio::task::spawn_blocking(move || {
            let mut cmd = std::process::Command::new(&argv[0]);
            cmd.args(&argv[1..])
                .env(norte_frontend::shell::LEVEL_VAR, level)
                .stdin(stdio())
                .stdout(stdio())
                .stderr(stdio());
            if let Some(dir) = cwd {
                cmd.current_dir(dir);
            }
            cmd.status().map(Some)
        })
        .await
    };
    // La espera va DESPUÉS del hijo y ANTES de restaurar: es el hueco en el
    // que la salida del comando sigue en pantalla. Su propio fallo no puede
    // saltarse la restauración, así que se guarda y se propaga con el resto.
    let waited = if wait_for_key {
        wait_for_any_key(terminal.backend_mut()).await
    } else {
        Ok(())
    };
    // Lo que el usuario tecleó MIENTRAS corría el hijo sigue en el buffer de
    // crossterm, y sin drenarlo el run loop lo despacharía acto seguido como
    // comandos contra un listado que acaba de cambiar (review de S4,
    // MINOR-2): el resto de un pegado multilínea es el caso que duele.
    drain_type_ahead().await;
    let restored = resume_terminal(terminal, capture, raton);
    suspension_outcome(child, waited, restored)
}

/// Cede la terminal: suelta el ratón, el bracketed paste, sale del raw mode y
/// de la pantalla alternativa, en ese orden.
///
/// La captura se suelta la PRIMERA: el programa que viene detrás no la pidió,
/// y heredarla le mete cada movimiento del puntero por stdin como si fueran
/// teclas. El bracketed paste sigue el MISMO argumento (#143): el hijo no lo
/// pidió tampoco, y heredarlo le entregaría cada pegado envuelto en
/// `\e[200~`/`\e[201~` en vez de texto plano — `less` o un editor externo
/// leerían esos marcadores como si el usuario los hubiera tecleado. Escritura
/// síncrona a la terminal de control (`terminal.backend_mut()`, nunca
/// stdout — ver `tty.rs`), misma exención puntual de la regla 2 que el resto
/// de la suspensión.
/// # Errors
///
/// Lo que devuelva crossterm al soltar la captura, al salir del raw mode o al
/// dejar la pantalla alternativa. Un fallo aquí NO exime de restaurar: la
/// terminal puede haber quedado a medio ceder, y es justo el estado que
/// [`resume_terminal`] tiene que deshacer.
pub fn suspend_terminal(
    terminal: &mut tty::Tui,
    capture: &mut mouse::Capture,
) -> std::io::Result<()> {
    use crossterm::event::DisableBracketedPaste;
    use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};
    mouse::release_for_suspend(capture, terminal.backend_mut())?;
    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
    Ok(())
}

/// Recupera la terminal: pantalla alternativa, raw mode, bracketed paste, la
/// captura de ratón EXACTAMENTE como estaba (si el usuario la tenía apagada,
/// `[ui] mouse = false`, volver de un shell no se la enciende) y un
/// repintado limpio.
///
/// Bracketed paste, a diferencia del ratón, no tiene un `[ui]` que lo apague:
/// vuelve SIEMPRE, igual que el raw mode — norte lo pide en cuanto tiene la
/// terminal (`tty::init`), sin condición de usuario de por medio (#143).
/// # Errors
///
/// Lo que devuelva crossterm al volver a la pantalla alternativa, al pedir el
/// raw mode o al restituir la captura, y lo que devuelva el backend al
/// limpiar. Sin traducir: el llamante lo propaga detrás del resultado del
/// hijo.
pub fn resume_terminal(
    terminal: &mut tty::Tui,
    capture: &mut mouse::Capture,
    raton: bool,
) -> std::io::Result<()> {
    use crossterm::event::EnableBracketedPaste;
    use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
    use ratatui::backend::Backend as _;
    crossterm::execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableBracketedPaste
    )?;
    enable_raw_mode()?;
    mouse::restore_after_suspend(capture, raton, terminal.backend_mut())?;
    // NO `Terminal::clear()`, y esto no es una preferencia de estilo: en
    // ratatui 0.30 esa función pregunta por la posición del cursor
    // (`get_cursor_position` → `crossterm::cursor::position`), que emite el
    // DSR `ESC [ 6 n` por **stdout** — el stdout del proceso, no el writer de
    // nuestro backend. Bajo `--pick` stdout es la tubería de datos del
    // llamante, así que volver de una suspensión le inyectaba `\x1b[6n`
    // delante de la primera ruta del flujo terminado en NUL. Lo cazó la
    // verificación de extremo a extremo de S4, no la suite: es exactamente el
    // fallo que la tarea 1 existía para impedir, entrando por una puerta que
    // la tarea 1 no controla.
    //
    // Limpiar por el BACKEND escribe en `/dev/tty` como todo lo demás, y dos
    // `swap_buffers` dejan los DOS buffers en blanco, que es lo que fuerza un
    // repintado completo en el siguiente draw (uno solo dejaría el anterior
    // con el contenido de antes de suspender y el diff se comería casi todo).
    terminal.backend_mut().clear()?;
    terminal.swap_buffers();
    terminal.swap_buffers();
    Ok(())
}

/// Qué devuelve una suspensión cuando más de una cosa pudo fallar.
///
/// Extraído (review de S4, M6) porque es la ÚNICA parte de `run_suspended`
/// que se puede probar sin una terminal, y es donde vive la regla: el
/// resultado del hijo manda —es la respuesta a lo que el usuario pidió—, y
/// los fallos de la espera y de la restauración se propagan detrás de él en
/// ese orden. Un join roto se convierte en un error de I/O porque para el
/// caller es indistinguible de que el hijo no llegara a correr.
/// # Errors
///
/// No falla por sí misma: devuelve el primero de los tres que traiga error, en
/// el orden hijo → espera → restauración. Un `JoinError` del hijo se convierte
/// en `io::Error::other` porque para el llamante es indistinguible de que el
/// hijo no llegara a correr.
pub fn suspension_outcome(
    child: Result<std::io::Result<Option<std::process::ExitStatus>>, tokio::task::JoinError>,
    waited: std::io::Result<()>,
    restored: std::io::Result<()>,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    // El fallo del hijo se devuelve ANTES que los otros dos. `run_opener`
    // decía esto mismo en su comentario y hacía lo contrario (`restored?`
    // salía primero), lo cual nunca se notó porque restaurar no falla casi
    // nunca; al escribir el test la contradicción salió sola. Gana el hijo
    // porque es la respuesta a lo que el usuario pidió: «no existe ese
    // shell» es accionable y «no se pudo volver a la pantalla alternativa»
    // no dice nada sobre la tecla que se pulsó.
    let status = child.map_err(std::io::Error::other)??;
    waited?;
    restored?;
    Ok(status)
}

/// Se traga lo que el usuario tecleó mientras la terminal no era de norte.
///
/// No es cortesía: sin esto, el resto de un pegado multilínea (o cualquier
/// type-ahead) llega al run loop como pulsaciones y se despacha como COMANDOS
/// contra un listado que el hijo acaba de cambiar. Acotado a
/// [`TYPE_AHEAD_MAX`] eventos para que una tormenta de resize no lo convierta
/// en un bucle.
async fn drain_type_ahead() {
    let _ = tokio::task::spawn_blocking(|| {
        for _ in 0..TYPE_AHEAD_MAX {
            match crossterm::event::poll(std::time::Duration::ZERO) {
                Ok(true) => {
                    if crossterm::event::read().is_err() {
                        return;
                    }
                }
                _ => return,
            }
        }
    })
    .await;
}

/// Tope de eventos que [`drain_type_ahead`] descarta de una vez.
const TYPE_AHEAD_MAX: usize = 4096;

/// Pinta el aviso y bloquea hasta la siguiente pulsación, con la terminal ya
/// fuera del modo TUI.
///
/// Se lee por `crossterm::event::read`, no un byte crudo de la tty, porque un
/// byte crudo parte las secuencias de escape: una flecha entrega `ESC [ A` y
/// quedarse el `ESC` deja `[ A` en el buffer, que la TUI leerá acto seguido
/// como dos teclas que el usuario no pulsó. `read` parsea el evento entero.
/// Raw mode se enciende para que valga CUALQUIER tecla y no haga falta un
/// Enter (en modo canónico el terminal no entrega nada hasta el salto).
///
/// # Por qué no compite con el `EventStream` del run loop
///
/// No porque compartan el mutex de la fuente interna de crossterm —eso solo
/// serializa—, sino porque el hilo lector que `EventStream` levanta cuando lo
/// polean NO está vivo aquí: termina antes de entregar un evento, y todo
/// escritor de `pending_shell` es una tecla ya despachada, así que el run
/// loop está parado en `recv()` mientras esto corre. Es un invariante
/// INCIDENTAL, y conviene saberlo: el primer camino que deje una suspensión
/// pendiente sin venir de una tecla (un temporizador, un despacho desde Lua,
/// una acción de plugin) reintroduce la carrera y se comería teclas del
/// usuario para replicarlas después. Por lo mismo, esta espera no debe
/// envolverse jamás en un `select!` con timeout: el `spawn_blocking` no es
/// cancelable y se quedaría con el lock del lector.
///
/// Un error de lectura (sin stdin, terminal muerta) sale sin más: la espera
/// es cortesía y no puede convertirse en un cuelgue.
async fn wait_for_any_key(out: &mut impl std::io::Write) -> std::io::Result<()> {
    write_resume_prologue(out)?;
    crossterm::terminal::enable_raw_mode()?;
    let read = tokio::task::spawn_blocking(|| {
        loop {
            match crossterm::event::read() {
                Ok(crossterm::event::Event::Key(k))
                    if k.kind == crossterm::event::KeyEventKind::Press =>
                {
                    return;
                }
                // Resize/Mouse/Focus y las repeticiones no cuentan como «una
                // tecla»: seguir esperando.
                Ok(_) => {}
                // Sin stdin no hay tecla que esperar; salir en vez de girar.
                Err(_) => return,
            }
        }
    })
    .await;
    // El raw mode se queda encendido a propósito: `resume_terminal` lo vuelve
    // a pedir acto seguido y `enable_raw_mode` es idempotente.
    read.map_err(std::io::Error::other)
}

/// Devuelve la terminal a un estado conocido y escribe el aviso de «pulsa una
/// tecla».
///
/// El hijo acaba de tener la terminal entera y puede haberla dejado en
/// cualquier estado suyo: SGR activo, el juego de caracteres G1 de dibujo de
/// líneas seleccionado, el autowrap apagado (review de S4, L4). `clear()` y
/// el repintado de ratatui restituyen los atributos POR CELDA, pero no la
/// selección de juego de caracteres ni DECAWM — así que el aviso saldría en
/// rojo invertido y con glifos de caja, y el propio listado detrás. Se
/// emiten, en este orden: SGR reset, US-ASCII en G0, autowrap on.
///
/// El aviso lleva `\r\n` porque el raw mode que viene justo después ya no
/// traduce `\n`, y sin el retorno de carro la línea siguiente sale escalonada.
/// # Errors
///
/// Lo que devuelva `out` al escribir el prólogo o al hacer flush.
pub fn write_resume_prologue(out: &mut impl std::io::Write) -> std::io::Result<()> {
    write!(
        out,
        "\x1b[0m\x1b(B\x1b[?7h\r\n{}\r\n",
        t("msg-shell-press-key")
    )?;
    out.flush()
}
