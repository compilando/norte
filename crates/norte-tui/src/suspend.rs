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
    let _signals = {
        use tokio::signal::unix::{SignalKind, signal};
        (
            signal(SignalKind::interrupt()).ok(),
            signal(SignalKind::quit()).ok(),
        )
    };
    // El estado de la captura se lee ANTES de tocar nada: si la propia
    // liberación falla a mitad, la restauración tiene que saber a qué volver
    // (review de S4, MINOR-6).
    let mouse_on = capture.active();
    // ---- frontera: de aquí en adelante, todo camino pasa por `resume_terminal`.
    let yielded = suspend_terminal(terminal, capture);
    if let Err(e) = yielded {
        let _ = resume_terminal(terminal, capture, mouse_on);
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
            // El programa se resuelve a ruta ABSOLUTA aquí, con el cwd de
            // norte todavía puesto, y jamás se le entrega el nombre crudo a
            // `Command` (#302). En unix `current_dir` se aplica ANTES de
            // resolver el programa, así que un `$EDITOR=vim` con un `.` (o un
            // componente vacío) en el `PATH` ejecutaría un fichero llamado
            // `vim` dentro del directorio que el lector está NAVEGANDO:
            // extraer un archivo hostil, entrar y pulsar F4. `resolve_program`
            // se salta las entradas relativas del `PATH` por eso mismo.
            //
            // Y si no se encuentra, NO se lanza: caer al nombre crudo sería
            // devolverle la búsqueda a `execvp` con el cwd ya cambiado, que es
            // exactamente el agujero. Un editor que no está instalado daba un
            // ENOENT de todas formas; lo que cambia es que ahora el mensaje
            // dice qué programa.
            //
            // `split_first` y no `argv[0]`: el `is_empty` de arriba lo cubre
            // hoy, pero un índice es un panic y aquí ya hay un `io::Result`.
            let (nombre, resto) = argv.split_first().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "argv vacío")
            })?;
            let programa = norte_frontend::openers::resolve_program(nombre).ok_or_else(|| {
                // El programa ya lo NOMBRA `msg-shell-failed`, así que este
                // detalle dice solo lo que el llamante no sabe: que no se
                // encontró, y dónde se buscó.
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "not found in PATH (relative PATH entries are ignored)",
                )
            })?;
            let mut cmd = std::process::Command::new(&programa);
            cmd.args(resto)
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
    let restored = resume_terminal(terminal, capture, mouse_on);
    suspension_outcome(child, waited, restored)
}

/// Le CEDE la terminal al subshell persistente hasta que el lector la pida de
/// vuelta con el mismo acorde (#142).
///
/// Devuelve el directorio en el que el shell quedó, si lo anunció y es otro:
/// el panel lo sigue, que es la mitad de por qué un subshell no es un
/// scrollback.
///
/// # Cómo se reparten las teclas
///
/// Hay UN solo lector de la terminal —el de crossterm, el que la TUI ya usa— y
/// las teclas se TRADUCEN a los bytes que un shell espera
/// ([`crate::subshell::tecla_a_bytes`]). Un hilo leyendo `/dev/tty` en crudo
/// habría sido más fiel y habría dejado ese hilo bloqueado dentro de un `read`
/// al soltar el shell, comiéndose la siguiente tecla del lector: la que ya era
/// para los paneles.
///
/// El raw mode se queda PUESTO mientras dura: sin él la línea la cocina el
/// terminal y el shell no ve una tecla hasta el Enter — ni edición de línea,
/// ni Ctrl+C, ni historial.
///
/// # Bloquea
///
/// SÍNCRONA a propósito, y hay que llamarla desde
/// [`tokio::task::block_in_place`]: el bucle de abajo se queda dentro toda la
/// sesión de shell —minutos, si el lector dejó un `make` corriendo— haciendo
/// I/O bloqueante sobre la terminal. Un `async fn` que nunca cede sería la
/// regla 2 con otra firma, y sin `block_in_place` se llevaría por delante el
/// hilo del executor: las tasks de fondo (los drenadores paginados, el
/// watcher) dejarían de avanzar mientras el shell está delante.
///
/// # Errors
/// Lo que falle al ceder o recuperar la terminal, o al escribir la salida del
/// shell.
// POSIX, como el módulo `subshell` entero (ver `lib.rs`).
#[cfg(unix)]
pub fn attach_subshell(
    terminal: &mut tty::Tui,
    capture: &mut mouse::Capture,
    sub: &mut crate::subshell::Subshell,
    dir: &std::path::Path,
    acorde: norte_frontend::keymap::Chord,
) -> std::io::Result<Option<std::path::PathBuf>> {
    use crossterm::event::{Event, poll, read};
    use std::io::Write as _;

    let mouse_on = capture.active();
    // El handle a la terminal de control se abre ANTES de ceder nada
    // (`run_suspended` hace lo mismo, y por lo mismo): entre `suspend_terminal`
    // y `resume_terminal` no puede salirse, y un `?` aquí dejaba la pantalla
    // alternativa cerrada, el ratón suelto y el raw mode puesto, con el bucle
    // repintando encima del scrollback del lector.
    let mut salida = tty::open_controlling_terminal()?;
    if let Err(e) = suspend_terminal(terminal, capture) {
        let _ = resume_terminal(terminal, capture, mouse_on);
        return Err(e);
    }
    // Raw mode OTRA VEZ, que `suspend_terminal` lo quita: aquí no se lanza un
    // programa que se quede la terminal, se le pasan las teclas a mano.
    let raw = crossterm::terminal::enable_raw_mode();
    // El tamaño puede haber cambiado con los paneles delante, y el shell no se
    // enteró: sus programas a pantalla completa pintarían sobre una geometría
    // que ya no existe hasta que alguien redimensionara ESTANDO dentro.
    if let Ok(tam) = crossterm::terminal::size() {
        sub.redimensionar(tam);
    }
    // El punto de partida es dónde ESTÁ EL PANEL, no dónde estaba el shell: al
    // entrar se le manda ahí, así que comparar con su posición anterior daba
    // «cambió» —y un relistado del panel a donde ya estaba— en cada Ctrl+O.
    let antes = Some(dir.to_path_buf());
    // El shell SIGUE al panel al entrar. Es la otra mitad del seguimiento —la
    // de vuelta la hace quien llama con lo que esto devuelve. Puede NEGARSE
    // (una línea a medias, un `vim` delante): ver `Subshell::ir_a`.
    let _ = sub.ir_a(dir);
    let resultado = (|| -> std::io::Result<()> {
        loop {
            // El plazo corto es lo que hace que la salida del shell aparezca
            // mientras nadie teclea: sin él, un `make` no se vería avanzar
            // hasta la siguiente tecla.
            if poll(std::time::Duration::from_millis(20))? {
                match read()? {
                    // El acorde se compara CANÓNICO (`Chord`), no como evento
                    // crudo: el que ata `app.toggle-panels` sale del keymap, y
                    // dos eventos crossterm distintos —`KeyEventKind`, el
                    // `shift` que un `Char` ya lleva dentro— son el mismo
                    // acorde. Comparando eventos, la tecla de salir dependía
                    // de si el terminal manda repeticiones.
                    Event::Key(k)
                        if k.kind == crossterm::event::KeyEventKind::Press
                            && crate::keymap::chord_from_crossterm(k.modifiers, k.code)
                                == Some(acorde) =>
                    {
                        return Ok(());
                    }
                    Event::Key(k) => {
                        if k.kind == crossterm::event::KeyEventKind::Press
                            && let Some(bytes) = crate::subshell::tecla_a_bytes(&k)
                        {
                            let _ = sub.escribir(&bytes);
                        }
                    }
                    // El shell tiene que saber el tamaño nuevo o pinta sobre
                    // una pantalla que no existe.
                    Event::Resize(w, h) => sub.redimensionar((w, h)),
                    // Un pegado SÍ llega, aunque el ratón no: el argumento de
                    // «un shell no lo pide» se cae en cuanto el shell tiene un
                    // `vim` delante, que sí lo pidió — y el pegado se perdía
                    // entero, sin error y sin dejar la mitad.
                    Event::Paste(texto) => {
                        let _ = sub.escribir(texto.as_bytes());
                    }
                    _ => {}
                }
            }
            let pendiente = sub.drenar();
            if !pendiente.is_empty() {
                salida.write_all(&pendiente)?;
                salida.flush()?;
            }
            if sub.muerto() {
                return Ok(());
            }
        }
    })();
    // La restauración pasa SIEMPRE, como en `run_suspended`: el error del
    // bucle se propaga detrás.
    if raw.is_ok() {
        let _ = crossterm::terminal::disable_raw_mode();
    }
    let vuelta = resume_terminal(terminal, capture, mouse_on);
    resultado?;
    vuelta?;
    // Solo si CAMBIÓ: devolver el mismo directorio haría que cada Ctrl+O
    // relistara el panel para nada.
    //
    // La comparación NORMALIZA, aunque lo que se devuelve son los bytes de
    // verdad (pitfall de macOS, CLAUDE.md). `$PWD` es la cadena que el shell
    // recibió en el `cd`, no una re-lectura del disco: en macOS un lector que
    // teclea `cd ~/Documentos/café` deja un `$PWD` en NFC mientras el `VPath`
    // que norte sacó del `readdir` de ese mismo directorio está en NFD. Byte a
    // byte no coinciden nunca, así que cada Ctrl+O relistaba —y dejaba el
    // panel con un `VPath` que ni el historial, ni los favoritos, ni las
    // marcas reconocen como el de antes.
    let ahora = sub.cwd();
    let clave = |p: &Option<std::path::PathBuf>| {
        use std::os::unix::ffi::OsStrExt as _;
        p.as_ref().map(|p| {
            norte_encoding::name_key(p.as_os_str().as_bytes(), norte_encoding::FoldMode::None)
                .into_owned()
        })
    };
    Ok(if clave(&ahora) == clave(&antes) {
        None
    } else {
        ahora
    })
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
///
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
    // El protocolo de teclado de kitty (`[ui] alt_menu`), por lo mismo que la
    // captura: el shell no lo pidió y leería escapes en vez de letras.
    crate::alt_menu::ceder(terminal.backend_mut())?;
    // T4 (fase 5 WOW), momento 3 de 4: si el visor tenía una imagen
    // colocada, se borra ANTES de soltar la terminal — el programa que
    // viene detrás tampoco la pidió, y sin borrarla quedaría flotando sobre
    // su pantalla. Best-effort (nunca `?`): un fallo aquí no puede impedir
    // ceder la terminal, que es lo que este momento existe para garantizar.
    // Al volver ([`resume_terminal`]) no hace falta colocarla de vuelta a
    // mano: `app.viewer_imagen` sigue vivo, y el primer frame que el run
    // loop pinte tras la reanudación la vuelve a colocar solo (el mismo
    // mecanismo que cierra el visor o lo mueve a otro fichero).
    crate::kitty_graphics::borrar_colocada(terminal.backend_mut());
    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        // El CURSOR también se devuelve, y no estaba (#142): `ratatui` lo
        // esconde en cada frame que no fija una posición, y esta TUI no fija
        // ninguna. La pantalla alternativa NO guarda ese estado, así que el
        // programa de detrás heredaba un cursor invisible. Con el scrollback
        // de antes no se notaba; en un shell donde se TECLEA es lo primero
        // que se nota. El siguiente `draw` lo vuelve a esconder solo.
        crossterm::cursor::Show,
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
    mouse_on: bool,
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
    mouse::restore_after_suspend(capture, mouse_on, terminal.backend_mut())?;
    crate::alt_menu::recuperar(terminal.backend_mut())?;
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

#[cfg(test)]
mod suspend_tests {
    use super::run_suspended;
    use crate::app::{App, Modal, Pane};
    use crate::gestures::{shell_remote_message, submit_command_line};
    use norte_proto::VPath;

    fn app_en(wire: &str) -> App {
        let d = VPath::parse(wire).expect("wire de test");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    /// La terminal de control para los dos tests que suspenden de verdad, o
    /// `None` con un aviso — jamás un salto mudo (estilo de los saltos de
    /// wasm/MinIO).
    ///
    /// Tres condiciones, y las tres hacen falta:
    ///
    /// - `NORTE_TTY_TESTS`: sin el opt-in no se corre. Meter la terminal del
    ///   desarrollador en la pantalla alternativa y en raw mode a mitad del
    ///   gate es peor que no tener el test.
    /// - una `/dev/tty` que abra: sin ella no hay nada que suspender.
    /// - un **stdin** que sea terminal. Esto no es celo: `Terminal::clear`
    ///   (ratatui 0.30) pregunta la posición del cursor con un DSR y ESPERA
    ///   la respuesta POR STDIN. Bajo nextest stdin es `/dev/null` aunque el
    ///   proceso corra dentro de tmux, así que la respuesta no llega nunca y
    ///   la suspensión falla a los dos segundos por algo que no es un bug del
    ///   producto. Comprobarlo aquí es lo que impide que ese artefacto se lea
    ///   como un fallo.
    fn tty_for_test() -> Option<crate::tty::TtyOut> {
        use std::io::IsTerminal as _;
        if std::env::var_os("NORTE_TTY_TESTS").is_none() {
            eprintln!("skip: NORTE_TTY_TESTS unset (this one drives a real terminal)");
            return None;
        }
        if !std::io::stdin().is_terminal() {
            eprintln!("skip: stdin is not a terminal (the DSR of `clear` would never be answered)");
            return None;
        }
        match crate::tty::open_controlling_terminal() {
            Ok(out) => Some(out),
            Err(e) => {
                eprintln!("skip: no controlling terminal ({e})");
                None
            }
        }
    }

    /// Un pane remoto NO abre shell: la negativa es que la conversión a ruta
    /// nativa falle, y el mensaje NOMBRA el pane para que no parezca que la
    /// tecla está rota.
    #[test]
    fn un_pane_remoto_no_tiene_donde_poner_un_shell() {
        let app = app_en("sftp://host/x");
        assert!(
            norte_vfs_local::vpath_to_native(app.focused().dir()).is_err(),
            "si esta conversión llegara a funcionar, el brazo abriría un \
             shell en el sitio equivocado sin decir nada"
        );
        // `path_display` es quien decide la forma («⟨sftp host⟩/x»); lo que
        // este test pincha es que el pane SE NOMBRA, no el formato.
        let msg = shell_remote_message(&app);
        assert!(msg.contains("sftp") && msg.contains("host"), "{msg}");
    }

    /// El nombre de un directorio puede traer bidi/invisibles, y esta línea
    /// se pinta en la barra: sale SANEADA y con badge, jamás cruda.
    #[test]
    fn el_aviso_sanea_un_directorio_hostil() {
        // RLO dentro del nombre: el clásico para invertir lo que se lee.
        let app = app_en("sftp://host/a%E2%80%AEb");
        let msg = shell_remote_message(&app);
        assert!(
            !msg.contains('\u{202E}'),
            "el override bidi jamás llega a la barra: {msg:?}"
        );
        assert!(
            msg.contains(crate::ui::HOSTILE_BADGE),
            "y va marcado como hostil: {msg:?}"
        );
    }

    /// El prompt de `pane.command-line` devuelve la línea TAL CUAL (un
    /// espacio inicial es la convención `HISTCONTROL=ignorespace`, no basura
    /// que recortar) y una línea en blanco no lanza nada.
    #[test]
    fn la_linea_de_comandos_no_recorta_y_rechaza_lo_vacio() {
        let mut app = app_en("file:///tmp");
        app.open_command_line();
        for c in " make test".chars() {
            app.command_line_push(c);
        }
        assert_eq!(app.command_line_confirm().as_deref(), Some(" make test"));
        let mut app = app_en("file:///tmp");
        app.open_command_line();
        for c in "   ".chars() {
            app.command_line_push(c);
        }
        assert!(app.command_line_confirm().is_none());
        assert!(
            matches!(app.modal, Some(Modal::CommandLine { error: Some(_), .. })),
            "y el diagnóstico se queda bajo el campo"
        );
    }

    /// El Enter de la línea de comandos arma `$SHELL -c CMD` con la línea
    /// ENTERA como un solo argumento y el dir del pane como cwd.
    #[test]
    fn el_enter_de_la_linea_arma_shell_menos_c() {
        let mut app = app_en("file:///tmp");
        submit_command_line(&mut app, "ls | wc -l");
        let p = app.pending_shell.expect("deja la suspensión pendiente");
        assert_eq!(p.argv.len(), 3, "binario, -c y la línea: {:?}", p.argv);
        assert_eq!(p.argv[1], std::ffi::OsString::from("-c"));
        assert_eq!(
            p.argv[2],
            std::ffi::OsString::from("ls | wc -l"),
            "la línea no se trocea: la parsea el shell"
        );
        assert_eq!(p.cwd, Some(std::path::PathBuf::from("/tmp")));
        assert!(p.wait_for_key, "la salida tiene que poder leerse");
        assert!(app.modal.is_none(), "y el prompt se cierra");
    }

    /// El pane se fue a un remoto entre abrir el prompt y confirmarlo: no se
    /// ejecuta NADA (correrlo en el dir de norte sería hacerlo donde el
    /// usuario no está mirando), se avisa, y el prompt se cierra igual.
    #[test]
    fn una_linea_confirmada_sobre_un_pane_remoto_no_ejecuta_nada() {
        let mut app = app_en("sftp://host/x");
        submit_command_line(&mut app, "rm -rf .");
        assert!(app.pending_shell.is_none(), "nada que ejecutar");
        assert!(app.message.is_some(), "y se dice por qué");
        assert!(app.modal.is_none());
    }

    /// La regla de qué error gana cuando fallan varias cosas, probada SIN
    /// terminal (review de S4, M6). El efecto —quién toca la pantalla— pide
    /// una tty; la POLÍTICA no, y es donde vive lo que puede equivocarse.
    #[test]
    fn el_resultado_del_hijo_manda_sobre_la_espera_y_la_restauracion() {
        use std::io::{Error, ErrorKind};
        let ok_status = || {
            // Un `ExitStatus` real sin lanzar nada: el de un hijo trivial.
            std::process::Command::new("true")
                .status()
                .expect("`true` existe en cualquier unix")
        };
        // Todo bien: sale el status del hijo.
        let r = super::suspension_outcome(Ok(Ok(Some(ok_status()))), Ok(()), Ok(()));
        assert!(r.expect("ok").is_some());

        // Sin hijo (argv vacío) tampoco es un error.
        assert!(
            super::suspension_outcome(Ok(Ok(None)), Ok(()), Ok(()))
                .expect("ok")
                .is_none()
        );

        // El error del hijo GANA al de la espera y al de la restauración: es
        // la respuesta a lo que el usuario pidió.
        let e = super::suspension_outcome(
            Ok(Err(Error::new(ErrorKind::NotFound, "no shell"))),
            Err(Error::other("espera")),
            Err(Error::other("restore")),
        )
        .expect_err("el hijo falló");
        assert_eq!(e.kind(), ErrorKind::NotFound, "{e}");

        // Sin fallo del hijo, la espera va delante de la restauración.
        let e = super::suspension_outcome(
            Ok(Ok(None)),
            Err(Error::other("espera")),
            Err(Error::other("restore")),
        )
        .expect_err("falló la espera");
        assert!(e.to_string().contains("espera"), "{e}");

        // Y un fallo SOLO de la restauración se propaga: dejar la terminal a
        // medias jamás se traga.
        let e = super::suspension_outcome(Ok(Ok(None)), Ok(()), Err(Error::other("restore")))
            .expect_err("falló la restauración");
        assert!(e.to_string().contains("restore"), "{e}");
    }

    /// El aviso que precede a «pulsa una tecla» DEVUELVE la terminal a un
    /// estado conocido antes de escribir nada (review de S4, L4): el hijo
    /// pudo dejar SGR activo, el juego G1 de dibujo de líneas seleccionado o
    /// el autowrap apagado, y `clear()` restituye atributos por celda pero no
    /// esas tres cosas. Comprobable sin tty porque el prólogo escribe en
    /// cualquier `Write`.
    #[test]
    fn el_prologo_resetea_la_terminal_antes_del_aviso() {
        let mut out: Vec<u8> = Vec::new();
        super::write_resume_prologue(&mut out).expect("escribe en un Vec");
        let s = String::from_utf8(out).expect("UTF-8");
        assert!(s.starts_with("\x1b[0m"), "SGR reset primero: {s:?}");
        assert!(s.contains("\x1b(B"), "US-ASCII en G0: {s:?}");
        assert!(s.contains("\x1b[?7h"), "autowrap on: {s:?}");
        assert!(
            s.contains("\r\n"),
            "con retorno de carro: el raw mode que viene ya no traduce \\n"
        );
    }

    /// La suspensión restaura la terminal en TODOS los caminos, incluido el
    /// que es fácil de olvidar: un hijo que FALLA. Que la función devuelva
    /// —con el status del hijo dentro— es la prueba de que el fallo no
    /// cortocircuitó la restauración.
    ///
    /// Necesita una terminal de control DE VERDAD, así que es opt-in
    /// (`NORTE_TTY_TESTS=1`): correrla en el gate metería a la terminal del
    /// desarrollador en la pantalla alternativa y en raw mode a mitad de la
    /// suite. Salta con un aviso, en el estilo de los saltos de wasm/MinIO,
    /// jamás en silencio.
    #[tokio::test]
    async fn un_hijo_que_falla_no_se_salta_la_restauracion() {
        let Some(out) = tty_for_test() else { return };
        let mut term = crate::tty::init(out).expect("init");
        let mut capture = crate::mouse::Capture::default();
        let status = run_suspended(
            &mut term,
            &mut capture,
            vec![std::ffi::OsString::from("false")],
            None,
            false,
        )
        .await
        .expect("la suspensión devuelve");
        let _ = crate::tty::restore(&mut term);
        let status = status.expect("un argv no vacío tiene status");
        assert!(
            !status.success(),
            "`false` falla, y eso es lo que se propaga"
        );
    }

    /// Un argv VACÍO no lanza nada y no es un error: es `app.toggle-panels`.
    #[tokio::test]
    async fn un_argv_vacio_no_lanza_nada() {
        let Some(out) = tty_for_test() else { return };
        let mut term = crate::tty::init(out).expect("init");
        let mut capture = crate::mouse::Capture::default();
        let status = run_suspended(&mut term, &mut capture, Vec::new(), None, false)
            .await
            .expect("la suspensión devuelve");
        let _ = crate::tty::restore(&mut term);
        assert!(status.is_none(), "no hubo hijo, así que no hay status");
    }
}
