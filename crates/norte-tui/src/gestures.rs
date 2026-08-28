//! Los gestos que ceden el control a otro programa o mueven un panel entero:
//! openers, la línea de comandos, el editor, desconectar, el espejo y el
//! arrastre entre paneles.
//!
//! Vivían en el root del binario `ntc` —un crate DISTINTO de esta lib—, y con
//! ellos tres módulos de test de los grandes: `pane_gestures_tests` (682
//! líneas), `open_tests` y `edit_tests`.
//!
//! Lo que une a estas funciones es una forma, no un tema: TODAS deciden y
//! ninguna ejecuta. `resolve_opener` deja un `PendingOpen`, `submit_command_line`
//! y `edit_under_cursor` dejan un `PendingShell`, y `mirror_plan`/`pull_plan`
//! devuelven un [`PaneMove`] en vez de navegar. El dueño de la terminal —el
//! bucle de eventos— es quien lanza. Es lo que hace que la DECISIÓN se pueda
//! probar sin un `Backend` ni un flujo de eventos, y esos son justo los tests
//! que venían pegados a un binario.
//!
//! El rustdoc de [`shell_cwd`] estaba varado sobre `disconnect` en `main.rs`,
//! dos doc-comments seguidos delante de una sola función. Vuelve al suyo sin
//! tocar una palabra.

use crossterm::event::EventStream;
use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use norte_proto::{EntryKind, VPath};

use crate::app::{App, Trail, error_message};
use crate::mouse;
use crate::navigate::{Cd, cd_in};
use crate::suspend::run_suspended;
use crate::tty;

/// Resuelve el opener (#28) del fichero seleccionado y deja en
/// `app.pending_open` lo que el run loop —dueño de la terminal— lanzará.
/// Primero manda `ns.toml`; sin regla para ese mimetype queda el lanzador
/// del escritorio, que es lo que hace que F4 funcione sin haber escrito
/// configuración. Cada fallo va a la barra —degradación limpia, jamás un
/// lanzamiento a ciegas—: sin fichero (no-op), remoto o dentro de un archivo
/// (`msg-open-remote`), o binario ausente (`msg-open-missing-program`, que
/// también cubre un Linux sin `xdg-utils`).
pub fn resolve_opener(app: &mut App) {
    use norte_frontend::openers;
    let Some(path) = app
        .focused()
        .selected()
        .filter(|e| matches!(e.kind, EntryKind::File | EntryKind::Symlink))
        .map(|e| e.path.clone())
    else {
        return;
    };
    // Ruta nativa: SOLO `file://` local; archive/sftp/s3 → sin path nativo.
    let Ok(native) = norte_vfs_local::vpath_to_native(&path) else {
        app.message = Some(t("msg-open-remote"));
        return;
    };
    let mime = openers::guess_mime(
        path.file_name()
            .map_or(&[][..], norte_proto::Segment::as_bytes),
    );
    let Some(opener) = app.openers.resolve(mime) else {
        // Sin regla en `ns.toml` para este mimetype queda el último recurso:
        // el lanzador del propio escritorio. Antes esto era un mensaje de
        // error, lo que obligaba a escribir configuración para abrir un PDF.
        // Va `detached` — entrega el fichero al programa asociado y vuelve,
        // así que suspender la TUI solo pintaría un parpadeo.
        let (program, argv) = openers::system_opener(&native);
        app.pending_open = Some(crate::app::PendingOpen {
            program,
            argv,
            detached: true,
            // El del pane también aquí (#144): `xdg-open` se lo pasa al
            // programa asociado, que puede ser el mismo editor que un opener
            // declarado — heredar el cwd de norte por qué camino se llegó
            // sería la misma sorpresa con otra puerta.
            cwd: norte_vfs_local::vpath_to_native(app.focused().dir()).ok(),
        });
        return;
    };
    let program = opener.program().to_owned();
    // La sonda del binario en el PATH (`program_available`) es I/O de disco:
    // NO se hace aquí (camino async) — el run loop la corre en spawn_blocking
    // junto al lanzamiento (regla 2).
    // `%d` = el directorio del pane (nativo); si por lo que sea no convierte,
    // el padre del propio fichero.
    let dir = norte_vfs_local::vpath_to_native(app.focused().dir()).unwrap_or_else(|_| {
        native
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default()
    });
    app.pending_open = Some(crate::app::PendingOpen {
        program,
        argv: opener.argv(&[&native], &dir),
        detached: false,
        // El MISMO `dir` que alimenta `%d`: el hijo abre en el directorio que
        // el lector está mirando (#144).
        cwd: Some(dir),
    });
}

/// Sondea el binario del opener en el PATH (#28) — I/O de disco en
/// `spawn_blocking`, JAMÁS en el executor async (regla 2) — y, si existe,
/// suspende el TUI y lo lanza. Devuelve el mensaje de barra LOCALIZADO del
/// resultado (binario ausente / lanzado / fallo de spawn).
pub async fn launch_opener(
    terminal: &mut tty::Tui,
    // Suspender la TUI cede la terminal ENTERA: la captura de ratón se
    // suelta antes y se restituye después ([`run_opener`]).
    capture: &mut mouse::Capture,
    pending: crate::app::PendingOpen,
) -> String {
    let crate::app::PendingOpen {
        program,
        argv,
        detached,
        cwd,
    } = pending;
    let prog = program.clone();
    let available =
        tokio::task::spawn_blocking(move || norte_frontend::openers::program_available(&prog))
            .await
            .unwrap_or(false);
    if !available {
        return ta("msg-open-missing-program", &[("program", &program)]);
    }
    if detached {
        return match spawn_detached(argv, cwd).await {
            Ok(()) => ta("msg-open-launched", &[("program", &program)]),
            Err(e) => ta(
                "msg-open-failed",
                &[("program", &program), ("error", &e.to_string())],
            ),
        };
    }
    // Invariante: `Opener::argv` siempre empuja el binario (`command[0]`), y
    // `parse` rechaza `command` vacío — así `argv[0]` nunca panica, y un argv
    // vacío aquí NO significa lo que significa en `run_suspended` (enseñar la
    // terminal y no lanzar nada), que sería un F4 mudo.
    debug_assert!(
        !argv.is_empty(),
        "el argv de un opener siempre trae el binario"
    );
    // El directorio del pane como cwd (#144). El `%d` de un opener declarado
    // ya viaja dentro del argv, así que esto no es para resolver rutas: es
    // para que un editor guarde, y un `:e` navegue, donde el lector está
    // mirando — lo que hacen los tres comandos de shell desde #135 y esto no.
    //
    // Decidido, no descubierto: el precio es que un opener que escriba una
    // ruta RELATIVA pasa a escribirla en el directorio del pane. Se aceptó
    // por ser la sorpresa menor de las dos.
    match run_suspended(terminal, capture, argv, cwd, false).await {
        Ok(_) => ta("msg-open-launched", &[("program", &program)]),
        Err(e) => ta(
            "msg-open-failed",
            &[("program", &program), ("error", &e.to_string())],
        ),
    }
}

/// Lanza el comando SIN tocar la terminal: el lanzador del escritorio
/// (`xdg-open`/`open`/`explorer.exe`) entrega el fichero al programa asociado
/// y termina, así que suspender la TUI para él sería un parpadeo gratis. El
/// stdio va a `null` — un lanzador hablador no puede escribir encima del
/// listado. El hijo se espera en segundo plano (regla 2: en `spawn_blocking`),
/// que es lo que lo entierra: sin ese `wait` quedaría zombi hasta que muriese
/// el propio norte.
async fn spawn_detached(
    argv: Vec<std::ffi::OsString>,
    cwd: Option<std::path::PathBuf>,
) -> std::io::Result<()> {
    debug_assert!(
        !argv.is_empty(),
        "el argv del lanzador del sistema siempre trae el binario"
    );
    let mut child = tokio::task::spawn_blocking(move || {
        let mut cmd = std::process::Command::new(&argv[0]);
        // `current_dir` solo si hay: pasar el cwd heredado explícitamente no
        // es lo mismo que no tocarlo, y aquí no hay nada mejor que heredar
        // (#144).
        if let Some(dir) = &cwd {
            cmd.current_dir(dir);
        }
        cmd.args(&argv[1..])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    })
    .await
    .map_err(std::io::Error::other)??;
    tokio::task::spawn_blocking(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// El Enter de [`Modal::CommandLine`](crate::app::Modal::CommandLine) (#135):
/// deja `$SHELL -c CMD` pendiente
/// y cierra el prompt.
///
/// El guard de localidad se REPITE aquí, no basta con el del despacho que
/// abrió el prompt: entre abrirlo y confirmarlo el pane puede haberse ido a
/// un `sftp://`, y correr la línea en el directorio de norte no es lo que se
/// pidió — sería ejecutarla en un sitio que el usuario no está mirando. El
/// prompt se cierra en los dos casos: dejarlo abierto tras un rechazo
/// invitaría a pulsar Enter otra vez contra el mismo rechazo.
///
/// La línea viaja como UN argumento: la parsea el shell (tuberías, comillas,
/// globs), nunca norte. Trocearla aquí sería inventar una gramática que no
/// coincide con la del shell que va a recibirla.
pub fn submit_command_line(app: &mut App, cmd: &str) {
    match shell_cwd(app) {
        Ok(dir) => {
            let shell = norte_frontend::shell::login_shell();
            app.pending_shell = Some(crate::app::PendingShell {
                // El flag lo decide `norte-frontend` por shell (regla 7):
                // `cmd.exe` no entiende `-c`.
                argv: norte_frontend::shell::shell_command_argv(&shell, cmd),
                cwd: Some(dir),
                wait_for_key: true,
            });
        }
        Err(msg) => app.message = Some(msg),
    }
    app.command_line_submitted();
}

/// A dónde va el panel con foco cuando su sesión se cierra.
///
/// La decisión la toma `norte-frontend` y la comparten los dos frontends:
/// el rastro hacia atrás saltándose la máquina que se cierra, y casa cuando
/// no queda nada. Vivía dos veces —aquí «a casa, siempre», y en la ventana el
/// rastro—, y la misma tecla dejaba el panel en dos sitios distintos.
///
/// Se decide ANTES de soltar la sesión, como en la ventana: después, la ruta
/// del panel ya no sirve de clave.
fn destino_tras_desconectar(app: &App, cerrada: &VPath) -> VPath {
    norte_frontend::nav::regreso_tras_desconectar(cerrada, app.history[app.focus()].trail())
        .unwrap_or_else(norte_frontend::shell::home_vpath)
}

/// Cierra la sesión del panel con foco y lo saca de ahí (#140).
///
/// Las dos mitades importan y en este orden: primero se suelta la sesión
/// —mientras la ruta del panel sigue siendo la remota, que es de donde sale la
/// clave— y después se navega. Al revés habría que recordar de dónde se venía.
///
/// En un panel LOCAL no hay nada que cerrar y se dice: una tecla que contesta
/// «hecho» sobre algo que no ha hecho nada enseña a no fiarse del mensaje.
pub async fn disconnect(app: &mut App, backend: &Backend) {
    let dir = app.focused().dir().clone();
    if norte_vfs_local::vpath_to_native(&dir).is_ok() {
        app.message = Some(t("msg-disconnect-local"));
        return;
    }
    let destino = destino_tras_desconectar(app, &dir);
    match backend.close_connection(&dir).await {
        Ok(cerrada) => {
            app.message = Some(t(if cerrada {
                "msg-disconnect-done"
            } else {
                "msg-disconnect-none"
            }));
            // El panel no puede quedarse mirando una conexión que acaba de
            // cerrarse. La navegación la pide el run loop en la siguiente
            // vuelta, como cualquier otra.
            app.pending_disconnect_dest = Some(destino);
        }
        Err(e) => app.message = Some(error_message(&e)),
    }
}

/// El editor sobre la entrada bajo el cursor (#133).
///
/// Un editor abre un FICHERO DEL SISTEMA: sobre un pane remoto no hay ninguno
/// que darle —bajarlo, editarlo y volverlo a subir es otra feature, con su
/// conflicto y su reversa—, así que se dice y no se abre nada. Es el mismo
/// guard, y el mismo mensaje, que el shell y la línea de comandos.
///
/// Sobre un directorio tampoco: quien quiera entrar tiene `nav.enter`, y
/// abrirle un editor a una carpeta es enseñarle al editor lo que no sabe.
/// # Errors
///
/// El mensaje YA LOCALIZADO de por qué no se abre nada, que el llamante manda a
/// la barra tal cual: no hay entrada bajo el cursor, es un directorio, o el pane
/// es remoto (y entonces sale por [`shell_remote_message`], con la ubicación
/// saneada).
pub fn edit_under_cursor(app: &App) -> Result<crate::app::PendingShell, String> {
    let Some(entry) = app.focused().selected() else {
        return Err(t("msg-edit-nothing"));
    };
    if entry.kind == norte_proto::EntryKind::Dir {
        return Err(t("msg-edit-not-a-file"));
    }
    let Ok(native) = norte_vfs_local::vpath_to_native(&entry.path) else {
        return Err(shell_remote_message(app));
    };
    // El cwd del hijo es el directorio que se está mirando, como con el shell:
    // un `:w otro.txt` del editor cae donde el humano está, no donde arrancó
    // norte.
    let cwd = norte_vfs_local::vpath_to_native(app.focused().dir())
        .ok()
        .and_then(|d| norte_frontend::shell::child_cwd(&d));
    Ok(crate::app::PendingShell {
        argv: norte_frontend::shell::editor_argv(&native),
        cwd,
        // Un editor de pantalla completa se despide él solo; esperar una tecla
        // después sería un paso de más entre guardar y volver a los paneles.
        wait_for_key: false,
    })
}

/// El editor sobre un fichero que el daemon ACABA de crear (#290).
///
/// El hermano de [`edit_under_cursor`] para la otra mitad de `pane.edit-new`:
/// la ruta no sale del cursor —el listado puede no haberse refrescado todavía,
/// y el cursor puede estar en cualquier sitio— sino de lo que se mandó crear.
///
/// El `cwd` sale del PADRE del fichero creado, no del pane con foco: entre el
/// submit y el desenlace el lector puede haber pulsado Tab o navegado, y un
/// `:w otro.txt` del editor debe caer donde está el fichero que se acaba de
/// crear — que es de lo que va el gesto — y no donde quedó el foco.
///
/// `None` si esa ruta no tiene forma nativa: no debería pasar (crear se rehúsa
/// sobre un pane remoto, al abrir el diálogo y otra vez al confirmarlo), pero
/// abrir un editor sobre lo que no se puede nombrar no es una alternativa. El
/// llamante lo DICE: media mitad del gesto perdida en silencio es la clase de
/// cosa que este comando vino a quitar.
#[must_use]
pub fn edit_created(path: &VPath) -> Option<crate::app::PendingShell> {
    let native = norte_vfs_local::vpath_to_native(path).ok()?;
    let cwd = native.parent().and_then(norte_frontend::shell::child_cwd);
    Some(crate::app::PendingShell {
        argv: norte_frontend::shell::editor_argv(&native),
        cwd,
        wait_for_key: false,
    })
}

/// El directorio de trabajo que le toca a un hijo lanzado desde el pane con
/// foco, o el mensaje LOCALIZADO de por qué no hay ninguno.
///
/// Dos negativas distintas, y decirlas por separado importa: el pane no es
/// local (`sftp://`, un bucket, dentro de un archivo — no hay directorio en
/// esta máquina), o lo es pero su forma nativa no se le puede dar a un hijo
/// (Windows: una ruta que solo existe con el prefijo `\\?\`, ver
/// [`norte_frontend::shell::child_cwd`]). Un solo mensaje para las dos
/// mandaría al usuario a buscar el problema en el sitio equivocado.
///
/// Nota de carrera (review de S4, MINOR-3): esto resuelve una RUTA, no un
/// descriptor, así que entre la comprobación y el `chdir` del hijo alguien
/// con permiso de escritura en el padre puede cambiar el directorio por un
/// symlink. Cerrarlo de verdad pide `openat`/`fchdir` y no lo hace ninguna
/// otra ruta de norte; está dicho en los límites honestos del tema de ayuda.
/// # Errors
///
/// El mensaje YA LOCALIZADO de la negativa que toque, que son las dos de arriba
/// y se dicen por separado a propósito.
pub fn shell_cwd(app: &App) -> Result<std::path::PathBuf, String> {
    let Ok(native) = norte_vfs_local::vpath_to_native(app.focused().dir()) else {
        return Err(shell_remote_message(app));
    };
    norte_frontend::shell::child_cwd(&native).ok_or_else(|| {
        let (text, hostile) = norte_frontend::path_display(app.focused().dir());
        ta(
            "msg-shell-cwd-unsupported",
            &[("path", &badged(&text, hostile))],
        )
    })
}

/// La ruta ya saneada, con el badge hostil FUERA de la traducción (para que
/// ningún locale pueda perderlo) y ACOTADA con elipsis media.
///
/// El tope no es cosmético (review de S4, M4): estos mensajes interpolan la
/// ruta A MITAD de la frase, y tanto la barra de estado (un `Paragraph` de
/// una línea) como el flash de la GUI cortan por la derecha sin marca — así
/// que una ruta larga se lleva por delante justo la parte que explica por qué
/// la tecla no hizo nada, y la tecla parece rota.
fn badged(text: &str, hostile: bool) -> String {
    let short = norte_frontend::middle_ellipsis(text, SHELL_MSG_PATH_MAX);
    if hostile {
        format!("{} {short}", crate::ui::HOSTILE_BADGE)
    } else {
        short
    }
}

/// Presupuesto en chars de la ruta dentro de un aviso de shell: deja sitio de
/// sobra para la cláusula que viene detrás en un terminal de 80 columnas.
const SHELL_MSG_PATH_MAX: usize = 48;

/// El aviso de «aquí no cabe un shell» (#135), con la ubicación SANEADA.
///
/// El saneado no es opcional: el nombre de un directorio puede traer bidi,
/// invisibles o controles, y esta línea se pinta en la barra de estado —
/// donde `path_display` es exactamente la puerta por la que pasa todo lo
/// demás que viene del disco.
#[must_use]
pub fn shell_remote_message(app: &App) -> String {
    let (text, hostile) = norte_frontend::path_display(app.focused().dir());
    ta("msg-shell-remote", &[("path", &badged(&text, hostile))])
}

/// Where a pane gesture wants to send a pane.
///
/// The `*_plan` functions return this instead of navigating so the DECISION —
/// which pane travels, and where — is testable without a `Backend` or an
/// event stream. `None` from them means there is nothing to do, and WHY is
/// deliberately not this type's business: the dispatch arm decides whether
/// the reason deserves a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneMove {
    /// The pane that will navigate.
    pub pane: usize,
    /// Where it will go.
    pub dir: VPath,
}

/// `pane.mirror`: the UNFOCUSED pane goes where the focused one is, and the
/// focus stays put — the fastest way to line up a copy, because the
/// destination of `pane.copy` is whatever the other pane holds.
///
/// `None` when the focused pane is a virtual search listing (a list of hits
/// is not a location, so there is no origin to send) or when both panes are
/// already there — a redundant `cd` would re-list the other pane and slide
/// its listing out from under the reader's cursor for nothing.
///
/// "Already there" reads `virtual_search` as well as the directory: a results
/// pane's `dir()` is the ROOT the search walked, which is usually the very
/// directory the other pane is sitting in, and it is NOT what the reader is
/// looking at. Comparing the two alone refused the gesture in silence
/// precisely when it had the most to do — the real cd is what takes the pane
/// out of search mode.
#[must_use]
pub fn mirror_plan(app: &App) -> Option<PaneMove> {
    let from = app.focus();
    let to = from ^ 1;
    if app.panes[from].virtual_search {
        return None;
    }
    let dir = app.panes[from].dir().clone();
    (app.panes[to].dir() != &dir || app.panes[to].virtual_search)
        .then_some(PaneMove { pane: to, dir })
}

/// `pane.mirror-target`: like [`mirror_plan`], but what travels is the
/// CURSOR's target — the folder under the cursor when it is one, and this
/// pane's own location otherwise.
///
/// It is Krusader's `Ctrl+←`/`Ctrl+→`, and the reason it is a separate verb
/// rather than a smarter `pane.mirror` is that `pane.mirror` is bound in four
/// presets as "send this location": teaching it to prefer the cursor would
/// change, in silence, what a key those readers already use does.
///
/// Which directory that is gets decided ONCE, in
/// [`norte_frontend::PaneState::target_dir`], so the window cannot answer it
/// differently (ADR 0077). The two refusals are [`mirror_plan`]'s, unchanged:
/// a virtual search listing has no location to send, and a destination that is
/// already there is left alone.
#[must_use]
pub fn mirror_target_plan(app: &App) -> Option<PaneMove> {
    let from = app.focus();
    let to = from ^ 1;
    if app.panes[from].virtual_search {
        return None;
    }
    let dir = app.panes[from].target_dir().clone();
    (app.panes[to].dir() != &dir || app.panes[to].virtual_search)
        .then_some(PaneMove { pane: to, dir })
}

/// `pane.pull`: the FOCUSED pane goes where the other one is — the same
/// gesture as [`mirror_plan`] the other way round, with the same two reasons
/// to decline and the same reading of a virtual DESTINATION.
#[must_use]
pub fn pull_plan(app: &App) -> Option<PaneMove> {
    let to = app.focus();
    let from = to ^ 1;
    if app.panes[from].virtual_search {
        return None;
    }
    let dir = app.panes[from].dir().clone();
    (app.panes[to].dir() != &dir || app.panes[to].virtual_search)
        .then_some(PaneMove { pane: to, dir })
}

/// Carries out what a `*_plan` decided: navigate, or explain the refusal.
///
/// `pane.mirror` and `pane.pull` differ ONLY in which pane travels and which
/// one the location is read FROM, and both of those are already settled by
/// the time the plan exists — so they share this body rather than two arms
/// that must be kept in step by hand.
///
/// `origin` is the pane the location comes from. A virtual search listing
/// there is the one refusal that deserves a message: the reader asked for
/// something that cannot be done. Both panes already being in the same place
/// stays SILENT — nothing was asked for that failed.
pub async fn run_pane_gesture(
    app: &mut App,
    backend: &Backend,
    events: &mut EventStream,
    plan: Option<PaneMove>,
    origin: usize,
) -> Cd {
    let Some(m) = plan else {
        if app.panes[origin].virtual_search {
            app.message = Some(t("msg-pane-not-a-location"));
        }
        return Cd::Cancelled;
    };
    // Navegación ORDINARIA, pero por `cd_in` y no por el envoltorio `cd`: el
    // pane que viaja lo dice el plan, y en el espejo NO es el del foco.
    cd_in(app, backend, events, m.pane, m.dir, Trail::Record).await
}

/// A fingerprint of WHICH surface owns the keyboard, sampled before and after
/// each turn of a repeated dispatch.
///
/// A count repeats the dispatch, and a dispatched command can put a modal, a
/// viewer or one of seven overlays in front of the panes. Everything left of
/// the count would then fire BEHIND it, against a pane the reader is no
/// longer looking at and cannot see change. The run loop routes a key event
/// by testing exactly these fields, so comparing them is the same question
/// the router asks.
///
/// It is COMPARED, never merely tested: `5` then `viewer.down` starts with
/// the viewer already open, and a guard that broke on "a viewer is open"
/// would stop that count after one row. Only a CHANGE means the dispatch
/// moved the keyboard.
#[must_use]
pub fn keyboard_owner(app: &App) -> u16 {
    let bits = [
        app.menu.is_some(),
        app.modal.is_some(),
        app.viewer.is_some(),
        app.help.is_some(),
        app.theme_picker.is_some(),
        app.columns_picker.is_some(),
        app.extensions.is_some(),
        app.nav_popup.is_some(),
        app.search_dialog.is_some(),
        // The diff pane (`Shift+F2`): a full keyboard owner while it is up,
        // like the viewer and unlike the live-search pane. Its rows are not
        // entries, so nothing behind it could act on what the cursor is on.
        app.compare.is_some(),
        app.palette.is_some(),
        app.settings.is_some(),
        // K3c: el editor de atajos. Como los demás overlays, y no como el
        // panel which-key de abajo: se queda TODAS las teclas mientras está
        // abierto, así que un despacho que lo abriera por detrás de una cuenta
        // dejaría el resto de las repeticiones cayendo en él.
        app.shortcuts.is_some(),
        app.focused().quick_visible().is_some(),
        // K3a: the which-key panel takes no keys — the pane resolver keeps
        // them while it is up — and, unlike its neighbours, this bit is
        // CONSTANT across the comparison by construction: `Resolution::Run`
        // clears the panel before `owner_before` is sampled, and nothing
        // reachable from `dispatch` can open one (the only two writers are
        // `App::show_pending`/`clear_pending`, both of them on the key path).
        // So it can never break a `5j`, and it can never save one either. It
        // is here as a DEFENSIVE entry: the day a command opens a which-key of
        // its own (a "show me everything" key is the obvious candidate), the
        // count must notice, and the alternative is remembering to add it
        // then.
        app.which_key.is_some(),
    ];
    bits.iter()
        .enumerate()
        .fold(0u16, |acc, (i, &on)| acc | (u16::from(on) << i))
}

#[cfg(test)]
mod pane_gestures_tests {
    use super::{App, Cd, Trail, keyboard_owner, mirror_plan, mirror_target_plan, pull_plan};
    use crate::app::{Modal, Palette, Pane, TrailStep};
    use crate::jobs::on_search_dialog_key;
    use crate::keymap::Command;
    use crate::navigate::{record_step, settle_suspended_trail};
    use crate::trail::{
        Rewind, back_target, forward_target, nav_stalled, rewind_for, rewind_trail,
    };
    use crate::viewer::Viewer;
    use norte_proto::{Error, VPath};

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire de test")
    }

    /// `App` con cada pane sobre su dir. Se construyen los panes ENTEROS
    /// (`Pane::new`) en vez de mover un pane existente: es el mismo molde que
    /// usan los demás módulos de test de este fichero y no hace falta ningún
    /// setter `#[cfg(test)]` nuevo.
    fn app_en(left: &str, right: &str) -> App {
        App::new(
            Pane::new(vp(left), Vec::new()),
            Pane::new(vp(right), Vec::new()),
        )
    }

    /// Espejo: el pane SIN foco se va a donde está el que tiene el foco, y el
    /// foco no se mueve.
    #[test]
    fn el_espejo_manda_al_otro_pane_y_no_mueve_el_foco() {
        let mut app = app_en("mem:///a", "mem:///b");
        app.set_focus(0);
        let plan = mirror_plan(&app).expect("con dos panes normales hay plan");
        assert_eq!(plan.pane, 1, "viaja el OTRO pane");
        assert_eq!(plan.dir, vp("mem:///a"), "a donde está el del foco");
        assert_eq!(app.focus(), 0, "el foco no se ha movido");
    }

    /// El espejo mira al FOCO, no al pane 0: con el foco a la derecha viaja
    /// el izquierdo. (Mutación de control: fijar `from = 0` rompe aquí.)
    #[test]
    fn el_espejo_con_el_foco_a_la_derecha_manda_el_izquierdo() {
        let mut app = app_en("mem:///a", "mem:///b");
        app.set_focus(1);
        let plan = mirror_plan(&app).expect("plan");
        assert_eq!(plan.pane, 0);
        assert_eq!(plan.dir, vp("mem:///b"));
    }

    /// El espejo del OBJETIVO manda la carpeta bajo el cursor, y sobre
    /// cualquier otra cosa —un fichero, la fila `..`— manda esta ubicación,
    /// que es lo que hace el espejo de siempre.
    #[test]
    fn el_espejo_del_objetivo_manda_la_carpeta_bajo_el_cursor() {
        use norte_proto::{Entry, EntryKind};
        let entrada = |wire: &str, kind| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: vp(wire),
            kind,
            size: None,
            mtime_ms: None,
        };
        let mut app = App::new(
            Pane::new(
                vp("mem:///a"),
                vec![
                    entrada("mem:///a/dentro", EntryKind::Dir),
                    entrada("mem:///a/f.txt", EntryKind::File),
                ],
            ),
            Pane::new(vp("mem:///b"), Vec::new()),
        );
        app.set_focus(0);

        let plan = mirror_target_plan(&app).expect("plan");
        assert_eq!(plan.pane, 1, "viaja el OTRO pane, como el espejo");
        assert_eq!(plan.dir, vp("mem:///a/dentro"), "la carpeta del cursor");

        app.panes[0].set_cursor(1);
        let plan = mirror_target_plan(&app).expect("plan");
        assert_eq!(
            plan.dir,
            vp("mem:///a"),
            "sobre un fichero, esta ubicación — como `pane.mirror`"
        );

        // Y `pane.mirror` NO cambia: sigue mandando la ubicación aunque el
        // cursor esté sobre una carpeta.
        app.panes[0].set_cursor(0);
        assert_eq!(mirror_plan(&app).expect("plan").dir, vp("mem:///a"));
    }

    /// Traer: el pane CON foco se va a donde está el otro.
    #[test]
    fn traer_mueve_el_pane_con_foco() {
        let mut app = app_en("mem:///a", "mem:///b");
        app.set_focus(0);
        let plan = pull_plan(&app).expect("plan");
        assert_eq!(plan.pane, 0);
        assert_eq!(plan.dir, vp("mem:///b"));
    }

    /// Los dos ya en el mismo sitio: no-op SILENCIOSO, no un cd redundante
    /// que reordene el listado del otro pane bajo el cursor del lector.
    #[test]
    fn en_el_mismo_dir_no_hay_nada_que_hacer() {
        let mut app = app_en("mem:///a", "mem:///a");
        app.set_focus(0);
        assert!(mirror_plan(&app).is_none());
        assert!(pull_plan(&app).is_none());
    }

    /// Desde un pane VIRTUAL de resultados no hay ubicación que mandar ni de
    /// donde traer: `dir()` ahí es la RAÍZ del walk, no lo que el lector ve.
    #[test]
    fn un_pane_virtual_no_es_una_ubicacion() {
        let mut app = app_en("mem:///a", "mem:///b");
        app.panes[0].virtual_search = true;
        app.set_focus(0);
        assert!(mirror_plan(&app).is_none(), "no hay origen que mandar");
        app.set_focus(1);
        assert!(pull_plan(&app).is_none(), "ni de donde traer");
    }

    /// El pane virtual solo veta cuando es el ORIGEN. Mandarle una ubicación
    /// ENCIMA sí vale: el cd real lo saca del modo búsqueda, que es
    /// exactamente lo que el lector pidió.
    #[test]
    fn un_pane_virtual_si_puede_ser_destino() {
        let mut app = app_en("mem:///a", "mem:///b");
        app.panes[1].virtual_search = true;
        app.set_focus(0);
        let plan = mirror_plan(&app).expect("el destino virtual no veta");
        assert_eq!(plan.pane, 1);
        assert_eq!(plan.dir, vp("mem:///a"));
    }

    /// Y el atajo de «ya están los dos en el mismo sitio» no puede callarse
    /// ante un destino VIRTUAL: la raíz por la que anduvo la búsqueda suele
    /// ser justamente el dir del otro pane, y ahí `dir()` no es lo que el
    /// lector está viendo. Con el atajo comparando solo dirs, reflejar sobre
    /// un pane de resultados enraizado ahí mismo no hacía NADA — ni sacaba al
    /// pane del modo búsqueda, ni decía por qué.
    #[test]
    fn un_destino_virtual_en_el_mismo_dir_si_tiene_algo_que_hacer() {
        let mut app = app_en("mem:///a", "mem:///a");
        app.panes[1].virtual_search = true;

        app.set_focus(0);
        let plan = mirror_plan(&app).expect("el destino virtual no está «ya ahí»");
        assert_eq!(plan.pane, 1, "viaja el pane de resultados");
        assert_eq!(plan.dir, vp("mem:///a"), "y el cd real lo saca del modo");

        // Y el gesto simétrico: con el foco EN el pane virtual, `pane.pull`
        // lo trae a donde está el otro — el mismo dir, listado de verdad.
        app.set_focus(1);
        let plan = pull_plan(&app).expect("traer a un pane virtual tampoco es no-op");
        assert_eq!(plan.pane, 1);
        assert_eq!(plan.dir, vp("mem:///a"));
    }

    // --- nav.back / nav.forward ---

    /// Mueve el pane 0 a `dir` sin pasar por un `cd` (que necesita backend):
    /// un pane NUEVO sobre ese dir, el mismo molde que usan los demás
    /// módulos de test de este fichero.
    fn poner_en(app: &mut App, dir: &str) {
        app.panes[0] = Pane::new(vp(dir), Vec::new());
    }

    /// El rastro se recorre de verdad: A→B→C, dos veces atrás llega a A. La
    /// oscilación A→B→A→B que daría recorrer la MRU es lo que este test
    /// rechaza.
    #[test]
    fn atras_recorre_el_rastro_y_adelante_lo_deshace() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///a"));
        app.history[0].record(vp("mem:///b"));

        let where_to = back_target(&mut app).expect("hay rastro");
        assert_eq!(where_to, vp("mem:///b"));
        poner_en(&mut app, "mem:///b");
        assert_eq!(back_target(&mut app), Some(vp("mem:///a")));
        poner_en(&mut app, "mem:///a");
        assert_eq!(back_target(&mut app), None, "se acabó el rastro");

        assert_eq!(forward_target(&mut app), Some(vp("mem:///b")));
    }

    /// Con el rastro vacío la tecla lo DICE: una tecla que calla es
    /// indistinguible de una rota. (El mensaje lo pone `walk_trail`, que
    /// necesita backend; aquí se pinea la mitad que decide que NO hay
    /// destino.)
    #[test]
    fn atras_sin_rastro_no_da_destino() {
        let mut app = app_en("mem:///a", "mem:///otro");
        app.set_focus(0);
        assert_eq!(back_target(&mut app), None);
        assert_eq!(forward_target(&mut app), None);
    }

    /// La propiedad que impide el bucle: un `Replay` no registra. Se
    /// comprueba sobre el rastro, que es donde vive la decisión.
    #[test]
    fn el_rastro_no_se_alimenta_de_si_mismo() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///b"));
        let before = app.history[0].back_len();
        let _ = back_target(&mut app);
        assert_eq!(
            app.history[0].back_len(),
            before - 1,
            "un paso atrás CONSUME rastro; jamás lo produce"
        );
    }

    /// Un paso atrás desde un pane de RESULTADOS sí se da (es la tecla que
    /// más se parece a «sácame de aquí»), y lo que deja en la rama de delante
    /// es el directorio REAL desde el que se buscó — no una lista de hits, que
    /// no es un sitio. El `dir()` de un pane virtual ES ese directorio.
    #[test]
    fn atras_desde_un_pane_de_resultados_deja_el_dir_real_en_la_rama() {
        let mut app = app_en("mem:///b", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///a")); // el lector llegó a B desde A
        app.panes[0].begin_search(vp("mem:///b")); // Alt+F7 en B
        assert!(app.panes[0].virtual_search, "pane de resultados");

        assert_eq!(
            back_target(&mut app),
            Some(vp("mem:///a")),
            "el paso sale de la búsqueda hacia donde el lector estaba antes"
        );
        poner_en(&mut app, "mem:///a"); // el cd real aterriza (y cosecha el run)
        assert_eq!(
            forward_target(&mut app),
            Some(vp("mem:///b")),
            "y el adelante devuelve al dir desde el que se buscó, como listado"
        );
    }

    /// Lo que hace honesto el test de arriba, pinchado en la costura que
    /// podría romperlo: la raíz de la búsqueda ES el `dir()` del pane que la
    /// lanza. Si el diálogo dejara teclear otra raíz, `back_target` empezaría
    /// a apuntar a un sitio donde el lector no ha estado y tendría que leer
    /// `SearchRun::prev_dir` en su lugar.
    #[test]
    fn la_raiz_de_la_busqueda_es_el_dir_del_pane_que_la_lanza() {
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut app = app_en("mem:///raiz", "mem:///otro");
        app.set_focus(0);
        let mut dialog = crate::app::SearchDialog::new();
        dialog.push_char('x'); // sin criterio, Enter no lanza
        app.search_dialog = Some(dialog);

        let params = on_search_dialog_key(&mut app, KeyModifiers::NONE, KeyCode::Enter)
            .expect("Enter con criterio lanza la búsqueda");
        assert_eq!(
            params.root,
            *app.panes[0].dir(),
            "la raíz del walk es el dir del pane con foco"
        );
    }

    /// El rastro es POR PANE: `back_target` sigue al foco, no al pane 0.
    #[test]
    fn el_rastro_es_del_pane_con_foco() {
        let mut app = app_en("mem:///izq", "mem:///der");
        app.history[0].record(vp("mem:///solo-izq"));
        app.set_focus(1);
        assert_eq!(back_target(&mut app), None, "el pane 1 no tiene rastro");
        app.set_focus(0);
        assert_eq!(back_target(&mut app), Some(vp("mem:///solo-izq")));
    }

    // --- La POLÍTICA del rastro (`rewind_for`) y su efecto (`rewind_trail`).
    // Los tests llaman a las MISMAS funciones que llama `walk_trail`: antes
    // re-implementaban el efecto (`untake_step` + `history.remove` a mano),
    // así que `walk_trail` podía dejar de rebobinar y seguían verdes.

    /// Un `NotFound` no solo rebobina: RETIRA el destino de todo el
    /// historial. Es la única de las tres decisiones que toca la MRU.
    #[test]
    fn la_politica_ante_un_destino_que_no_existe_es_rebobinar_y_retirar() {
        assert_eq!(
            rewind_for(&Cd::Failed(Error::NotFound)),
            Rewind::StepAndRetire
        );
    }

    /// Cualquier OTRO fallo rebobina pero CONSERVA el destino: un host caído
    /// o un directorio que no puedes leer siguen siendo sitios, y pueden
    /// responder al siguiente intento.
    #[test]
    fn la_politica_ante_otro_fallo_es_rebobinar_conservando_el_destino() {
        assert_eq!(
            rewind_for(&Cd::Failed(Error::PermissionDenied)),
            Rewind::Step
        );
    }

    /// Un cd ABANDONADO (Esc durante un listado lento, o el stream de
    /// eventos muriéndose) rebobina IGUAL que un fallo: nadie lo reanuda y el
    /// pane no se movió. Sin esto el rastro cree que el lector se fue de un
    /// directorio que sigue en pantalla.
    #[test]
    fn la_politica_ante_un_cd_abandonado_es_rebobinar() {
        assert_eq!(rewind_for(&Cd::Cancelled), Rewind::Step);
    }

    /// Y la ÚNICA que no toca el rastro: el TOFU va a reanudar ESTA misma
    /// navegación (el modal carga el pane y el modo de rastro), así que
    /// rebobinar contaría dos veces el reintento que sí funcione.
    #[test]
    fn la_politica_ante_un_cd_suspendido_es_no_tocar_el_rastro() {
        assert_eq!(rewind_for(&Cd::Suspended), Rewind::No);
    }

    /// Un cd que SÍ movió el pane no debe rebobinar nada, ni los desenlaces
    /// que llegan de otros caminos (refresh, swap) y jamás salen de un paso
    /// del rastro.
    #[test]
    fn un_cd_que_aterriza_no_rebobina() {
        assert_eq!(rewind_for(&Cd::Replaced(0)), Rewind::No);
        assert_eq!(rewind_for(&Cd::Refreshed([true, true])), Rewind::No);
        assert_eq!(rewind_for(&Cd::Swapped), Rewind::No);
    }

    // --- El freno del CONTADOR sobre el rastro (`nav_stalled`, ADR 0044).

    /// Un paso del rastro que no aterriza para el contador EN SECO. El paso
    /// se rebobina (los tests de arriba lo fijan), así que la vuelta
    /// siguiente pediría el MISMO listado: `20` + `nav.back` contra un host
    /// caído serían veinte llamadas remotas idénticas. Y `Esc` durante un
    /// listado ES `Cd::Cancelled`, de modo que sin este freno la tecla con la
    /// que el lector intenta pararlo alimentaría el reintento siguiente.
    #[test]
    fn un_paso_del_rastro_que_no_aterriza_para_el_contador() {
        for (label, outcome) in [
            ("abandonado", Cd::Cancelled),
            ("no existe", Cd::Failed(Error::NotFound)),
            ("sin permiso", Cd::Failed(Error::PermissionDenied)),
        ] {
            assert!(
                nav_stalled(Command::NavBack, &outcome),
                "atrás {label} debe parar"
            );
            assert!(
                nav_stalled(Command::NavForward, &outcome),
                "adelante {label} debe parar"
            );
        }
    }

    /// Un paso que SÍ aterriza deja seguir al contador: `3` + `nav.back` son
    /// tres pasos cuando los tres existen.
    #[test]
    fn un_paso_del_rastro_que_aterriza_deja_seguir_al_contador() {
        assert!(!nav_stalled(Command::NavBack, &Cd::Replaced(0)));
        assert!(!nav_stalled(
            Command::NavForward,
            &Cd::Refreshed([true, true])
        ));
        // Suspendido es el TOFU: lo para el cambio de dueño del teclado (el
        // modal), no este freno — y rebobinar aquí contaría dos veces el
        // reintento.
        assert!(!nav_stalled(Command::NavBack, &Cd::Suspended));
    }

    /// Y el freno es SOLO de los dos comandos del rastro. `Cd::Cancelled` es
    /// el desenlace por defecto de `dispatch`, así que todo comando que no es
    /// un cd lo devuelve: preguntar por él en general pararía `5j` en la
    /// primera fila.
    #[test]
    fn el_freno_del_rastro_no_alcanza_a_un_comando_que_no_navega() {
        assert!(!nav_stalled(Command::CursorDown, &Cd::Cancelled));
        assert!(!nav_stalled(Command::ViewerDown, &Cd::Cancelled));
    }

    /// El contador para cuando el despacho MUEVE el teclado a otra
    /// superficie. Se compara un antes con un después justamente porque el
    /// visor puede estar abierto DESDE EL PRINCIPIO (`5` + `viewer.down`): un
    /// guard que preguntara «¿hay visor?» mataría ese contador en la primera
    /// vuelta.
    #[test]
    fn el_dueno_del_teclado_cambia_cuando_un_comando_abre_algo() {
        let mut app = app_en("mem:///izq", "mem:///der");
        let solo_paneles = keyboard_owner(&app);
        app.modal = Some(Modal::ConfirmQuit);
        assert_ne!(
            keyboard_owner(&app),
            solo_paneles,
            "un modal se pone delante"
        );
        app.modal = None;
        assert_eq!(keyboard_owner(&app), solo_paneles, "y al cerrarlo vuelve");
        app.palette = Some(Palette::new(Vec::new()));
        assert_ne!(keyboard_owner(&app), solo_paneles, "la palette también");
        app.palette = None;
        // Y el caso que obliga a COMPARAR en vez de preguntar: con el visor
        // abierto desde el principio (`5` + `viewer.down`), el dueño no ha
        // cambiado entre vueltas y el contador tiene que seguir.
        app.viewer = Some(Viewer::new(
            vp("mem:///izq/x.txt"),
            b"hola\n".to_vec(),
            false,
        ));
        let con_visor = keyboard_owner(&app);
        assert_ne!(con_visor, solo_paneles, "el visor es otro dueño");
        assert_eq!(
            keyboard_owner(&app),
            con_visor,
            "pero no CAMBIA entre dos vueltas del contador"
        );
    }

    /// Un paso atrás que ATERRIZA en un cd fallido no puede dejar el rastro
    /// contando un movimiento que nunca ocurrió: el lector sigue donde
    /// estaba. Se rebobina ENTERO — el destino vuelve al rastro y el
    /// «adelante» no se queda con un fantasma que devolvería al lector al
    /// sitio del que no se ha movido.
    #[test]
    fn un_paso_atras_fallido_se_rebobina_entero() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///a"));
        app.history[0].record(vp("mem:///b"));
        let dir = back_target(&mut app).expect("hay rastro");
        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (1, 1)
        );

        let outcome = Cd::Failed(Error::PermissionDenied);
        rewind_trail(&mut app, 0, TrailStep::Back, &dir, rewind_for(&outcome));

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (2, 0),
            "el rastro queda exactamente como estaba"
        );
        assert_eq!(
            back_target(&mut app),
            Some(dir),
            "y el mismo destino sigue disponible para reintentarlo"
        );
    }

    /// Simétrico: un paso ADELANTE fallido se rebobina igual.
    #[test]
    fn un_paso_adelante_fallido_se_rebobina_entero() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///b"));
        let _ = back_target(&mut app); // rastro: back=[], fwd=[c]
        poner_en(&mut app, "mem:///b");
        let dir = forward_target(&mut app).expect("hay rama de delante");
        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (1, 0)
        );

        let outcome = Cd::Failed(Error::PermissionDenied);
        rewind_trail(&mut app, 0, TrailStep::Forward, &dir, rewind_for(&outcome));

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (0, 1)
        );
        assert_eq!(forward_target(&mut app), Some(dir));
    }

    /// El caso de esta revisión: `Esc` durante el listado del paso. El lector
    /// sigue en C, así que el rastro tiene que quedarse como estaba. Con el
    /// paso dado por bueno, el «adelante» siguiente cd-ea al directorio que
    /// ya está en pantalla (una tecla que no hace nada visible) y `back`
    /// hereda un fantasma que se come el siguiente `nav.back`.
    #[test]
    fn un_paso_abandonado_con_esc_no_deja_fantasma_en_el_rastro() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///a"));
        app.history[0].record(vp("mem:///b"));
        let dir = back_target(&mut app).expect("hay rastro");

        // El pane NO se movió: sigue en C (`poner_en` no se llama).
        rewind_trail(
            &mut app,
            0,
            TrailStep::Back,
            &dir,
            rewind_for(&Cd::Cancelled),
        );

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (2, 0),
            "el rastro queda como estaba: nadie se fue de C"
        );
        assert_eq!(
            back_target(&mut app),
            Some(vp("mem:///b")),
            "y el mismo destino sigue ahí para reintentarlo"
        );
    }

    /// El TOFU es la excepción: el modal REANUDA esta misma navegación, así
    /// que el paso ya dado se queda dado — rebobinarlo haría que el reintento
    /// exitoso contara dos veces.
    #[test]
    fn un_paso_suspendido_por_el_tofu_conserva_el_paso() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///a"));
        app.history[0].record(vp("mem:///b"));
        let dir = back_target(&mut app).expect("hay rastro");

        rewind_trail(
            &mut app,
            0,
            TrailStep::Back,
            &dir,
            rewind_for(&Cd::Suspended),
        );

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (1, 1),
            "el paso sigue dado: lo terminará el reintento"
        );
    }

    // --- El paso que sobrevive a quien lo empezó: TOFU (`Modal::TrustHostKey`).
    // `walk_trail` devuelve `Suspended` SIN rebobinar porque el reintento iba
    // a terminar el paso; estos tres pinchan quién lo termina de verdad, con
    // la misma pareja `rewind_for`/`rewind_trail` que corre en producción.

    /// El lector anduvo A→B→C→D, dio UN paso atrás (ya completo: está en C,
    /// con D en la rama de delante) y el SIGUIENTE paso atrás —hacia B— se
    /// queda suspendido en el modal TOFU. Devuelve la app y el destino del
    /// paso suspendido.
    ///
    /// La rama de delante previa no es decorado: es lo que distingue un
    /// rebobinado del rastro de dos. Con `fwd` vacío el segundo rebobinado
    /// sería un no-op y ningún test lo vería; con la rama del lector debajo,
    /// se la come.
    fn app_con_paso_suspendido() -> (App, VPath) {
        let mut app = app_en("mem:///d", "mem:///otro");
        app.set_focus(0);
        for dir in ["mem:///a", "mem:///b", "mem:///c"] {
            app.history[0].record(vp(dir));
        }
        // Un `nav.back` anterior YA completado: rastro back=[a,b], fwd=[d].
        let c = back_target(&mut app).expect("hay rastro");
        assert_eq!(c, vp("mem:///c"));
        poner_en(&mut app, "mem:///c");
        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (2, 1)
        );

        // Y AHORA el paso que el TOFU suspende: el pane NO se mueve.
        let dir = back_target(&mut app).expect("hay rastro");
        assert_eq!(dir, vp("mem:///b"));
        // Lo que hace `walk_trail` ante un `Suspended`: NADA, a propósito —
        // cuenta con que quien responda al modal termine el paso.
        rewind_trail(
            &mut app,
            0,
            TrailStep::Back,
            &dir,
            rewind_for(&Cd::Suspended),
        );
        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (1, 2),
            "el paso está dado y pendiente de terminar"
        );
        (app, dir)
    }

    /// El reintento ATERRIZA: el pane se movió de verdad, así que no se
    /// rebobina nada — el paso que `walk_trail` dio es el paso que ocurrió.
    #[test]
    fn un_reintento_que_aterriza_deja_el_paso_dado_y_no_lo_registra() {
        let (mut app, dir) = app_con_paso_suspendido();
        let before = (app.history[0].back_len(), app.history[0].fwd_len());
        let mru_before: Vec<VPath> = app.history[0].entries().iter().cloned().collect();
        // El modal TRANSPORTA el rastro de la navegación interrumpida, y el
        // reintento se lo pasa a `cd_in` tal cual: sigue siendo un `Replay`.
        let trail = Trail::Replay(TrailStep::Back);

        record_step(&mut app.history[0], &vp("mem:///c"), &dir, trail); // lo que hace el cd_in del reintento
        settle_suspended_trail(&mut app, 0, &dir, trail, &Cd::Replaced(0));

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            before,
            "el paso ya estaba contado: terminarlo no lo cuenta otra vez"
        );
        assert_eq!(
            app.history[0].entries().iter().cloned().collect::<Vec<_>>(),
            mru_before,
            "y un reintento que aterriza sigue siendo un Replay: no entra en la MRU"
        );
    }

    /// El reintento FALLA (o el lector deniega la clave, o confiar falla): la
    /// navegación muere sin que el pane se moviera nunca. El paso vuelve
    /// EXACTAMENTE una vez — rebobinarlo dos veces se comería la rama de
    /// delante que el lector ya tenía.
    #[test]
    fn un_reintento_abandonado_rebobina_el_paso_exactamente_una_vez() {
        let (mut app, dir) = app_con_paso_suspendido();
        let (back_dado, fwd_dado) = (app.history[0].back_len(), app.history[0].fwd_len());

        settle_suspended_trail(
            &mut app,
            0,
            &dir,
            Trail::Replay(TrailStep::Back),
            &Cd::Cancelled,
        );

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            (back_dado + 1, fwd_dado - 1),
            "el paso vuelve UNA vez: uno de más detrás, uno de menos delante \
             (dos rebobinados darían (3, 0) y se comerían la rama del lector)"
        );
        assert_eq!(
            back_target(&mut app),
            Some(dir),
            "y el mismo destino sigue disponible para reintentarlo"
        );
        assert_eq!(
            app.history[0].fwd_len(),
            2,
            "con la rama de delante que el lector ya tenía intacta debajo"
        );
    }

    /// El reintento se topa con OTRA clave desconocida: vuelve a suspenderse.
    /// No se rebobina (el modal nuevo carga el mismo rastro y el mismo paso,
    /// así que sigue habiendo quien lo termine) y tampoco se registra nada —
    /// sigue siendo un `Replay`.
    #[test]
    fn un_reintento_que_vuelve_a_suspenderse_no_rebobina_ni_registra() {
        let (mut app, dir) = app_con_paso_suspendido();
        let before = (app.history[0].back_len(), app.history[0].fwd_len());
        let mru_before: Vec<VPath> = app.history[0].entries().iter().cloned().collect();

        settle_suspended_trail(
            &mut app,
            0,
            &dir,
            Trail::Replay(TrailStep::Back),
            &Cd::Suspended,
        );
        // Y lo que el `cd_in` del reintento hace con el rastro: nada.
        record_step(
            &mut app.history[0],
            &vp("mem:///c"),
            &dir,
            Trail::Replay(TrailStep::Back),
        );

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            before,
            "el paso sigue pendiente de terminar, ni rebobinado ni duplicado"
        );
        assert_eq!(
            app.history[0].entries().iter().cloned().collect::<Vec<_>>(),
            mru_before,
            "y un Replay no entra en la MRU por reintentarse"
        );
    }

    /// Un cd NORMAL que se topa con el TOFU no tiene paso que rebobinar: no
    /// salió del rastro. `Trail::Record` lo dice, y `settle` no toca nada.
    #[test]
    fn un_cd_normal_suspendido_no_tiene_paso_que_rebobinar() {
        let (mut app, dir) = app_con_paso_suspendido();
        let before = (app.history[0].back_len(), app.history[0].fwd_len());

        settle_suspended_trail(&mut app, 0, &dir, Trail::Record, &Cd::Cancelled);

        assert_eq!(
            (app.history[0].back_len(), app.history[0].fwd_len()),
            before,
            "una navegación que no salió del rastro no le debe nada"
        );
    }

    /// Y si el destino resultó NO EXISTIR, además de rebobinar se RETIRA de
    /// todo el historial — el mismo trato que ya le da el popup a un
    /// `NotFound`. Sin esto `nav.back` seguiría apuntando a un directorio que
    /// acaba de demostrar que no está, y la tecla solo podría fallar.
    #[test]
    fn un_destino_notfound_desaparece_del_rastro_entero() {
        let mut app = app_en("mem:///c", "mem:///otro");
        app.set_focus(0);
        app.history[0].record(vp("mem:///a"));
        app.history[0].record(vp("mem:///b"));
        let dir = back_target(&mut app).expect("hay rastro");

        let outcome = Cd::Failed(Error::NotFound);
        rewind_trail(&mut app, 0, TrailStep::Back, &dir, rewind_for(&outcome));

        assert_eq!(
            back_target(&mut app),
            Some(vp("mem:///a")),
            "el atrás salta al siguiente vivo, no reintenta el dir muerto"
        );
        assert!(
            !app.history[0].entries().contains(&dir),
            "y tampoco sigue en la MRU que pinta el popup"
        );
    }
}

#[cfg(test)]
mod open_tests {
    use super::{App, resolve_opener};
    use crate::app::Pane;
    use norte_proto::{Entry, EntryKind, Segment, VPath};

    fn pane_con(nombre: &str) -> Pane {
        let dir = VPath::parse("file:///d").expect("wire de test");
        let entry = Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(nombre.as_bytes().to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        };
        Pane::new(dir, vec![entry])
    }

    /// Sin regla en `ns.toml` para el mimetype, F4 cae en el lanzador del
    /// escritorio en vez de rendirse con un mensaje. Antes esto obligaba a
    /// escribir configuración para abrir un PDF.
    #[test]
    fn sin_opener_declarado_cae_en_el_lanzador_del_sistema() {
        let mut app = App::new(pane_con("informe.pdf"), pane_con("otro.txt"));
        resolve_opener(&mut app);
        let pending = app.pending_open.expect("F4 resuelve algo que lanzar");
        assert!(
            pending.detached,
            "el lanzador del escritorio no suspende la TUI"
        );
        assert_eq!(pending.argv.len(), 2, "binario + fichero, sin shell");
        assert!(
            pending.argv[1].to_string_lossy().ends_with("informe.pdf"),
            "abre el fichero bajo el cursor: {:?}",
            pending.argv
        );
        assert!(app.message.is_none(), "y no deja un error en la barra");
    }

    /// Un opener declarado sigue mandando, y ese SÍ se queda con la terminal
    /// (puede ser `bat` o un editor).
    #[test]
    fn un_opener_declarado_gana_y_toma_la_terminal() {
        let mut app = App::new(pane_con("notas.txt"), pane_con("x.txt"));
        app.openers = norte_frontend::openers::OpenersConfig::parse(
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n",
        )
        .expect("config de test");
        resolve_opener(&mut app);
        let pending = app.pending_open.expect("F4 resuelve el opener declarado");
        assert_eq!(pending.program, "bat");
        assert!(!pending.detached);
    }

    /// #144: los DOS caminos de F4 llevan el directorio del pane como cwd.
    ///
    /// Los tres comandos de shell lo pasan desde #135 y los openers no, así
    /// que un editor abierto sobre un fichero del pane guardaba en el cwd de
    /// norte. Se dejó a propósito en la ola de shell —cambiarlo cambia
    /// comportamiento, y un opener que escriba una ruta RELATIVA pasa a
    /// escribirla en otro sitio— y se DECIDIÓ el 2026-08-14 pasarlo: la
    /// sorpresa de guardar donde no miras es la mayor de las dos.
    ///
    /// Los dos caminos y no solo el declarado: `xdg-open` entrega el fichero
    /// al programa asociado, que puede ser el mismo editor, y heredar el cwd
    /// según por qué puerta se llegó sería la misma sorpresa con otra cara.
    #[test]
    fn los_dos_caminos_de_f4_abren_en_el_directorio_del_pane() {
        for (nombre, config) in [
            ("informe.pdf", None),
            (
                "notas.txt",
                Some("[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n"),
            ),
        ] {
            let mut app = App::new(pane_con(nombre), pane_con("otro.txt"));
            if let Some(c) = config {
                app.openers =
                    norte_frontend::openers::OpenersConfig::parse(c).expect("config de test");
            }
            let expected = norte_vfs_local::vpath_to_native(app.focused().dir())
                .expect("el pane de test es local");
            resolve_opener(&mut app);
            let pending = app.pending_open.expect("F4 resuelve algo");
            assert_eq!(
                pending.cwd.as_deref(),
                Some(expected.as_path()),
                "{nombre}: el hijo abre donde el lector está mirando"
            );
        }
    }

    /// Un fichero remoto no tiene ruta nativa: ni opener declarado ni
    /// lanzador del sistema pueden abrirlo, y el usuario debe enterarse.
    #[test]
    fn un_fichero_remoto_no_lanza_nada() {
        let dir = VPath::parse("sftp://host/d").expect("wire de test");
        let entry = Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(b"a.pdf".to_vec()).unwrap()),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        };
        let mut app = App::new(Pane::new(dir.clone(), vec![entry]), Pane::new(dir, vec![]));
        resolve_opener(&mut app);
        assert!(app.pending_open.is_none());
        assert!(app.message.is_some(), "lo dice en la barra");
    }
}

#[cfg(test)]
mod disconnect_tests {
    use super::*;
    use crate::app::Pane;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire de test")
    }

    /// Un panel plantado en `sftp://srv/b` con el rastro que se le pasa.
    fn app_remota(rastro: &[&str]) -> App {
        let mut app = App::new(
            Pane::new(vp("sftp://srv/b"), Vec::new()),
            Pane::new(vp("file:///tmp"), Vec::new()),
        );
        let lado = app.focus();
        for p in rastro {
            app.history[lado].record(vp(p));
        }
        app
    }

    /// El destino sale del RASTRO, no de `$HOME`: es la misma decisión que
    /// toma la ventana, y cuando cada frontend la tomaba por su cuenta la
    /// misma tecla dejaba el panel en dos sitios distintos.
    #[test]
    fn el_destino_sale_del_rastro_como_en_la_ventana() {
        let app = app_remota(&["file:///home/o", "sftp://srv/a"]);
        assert_eq!(
            destino_tras_desconectar(&app, &vp("sftp://srv/b")),
            vp("file:///home/o"),
            "se salta lo de la máquina que se cierra"
        );
    }

    /// Sin nada ajeno en el rastro se cae a casa, que es lo que hace la
    /// ventana — y lo que hacía esta función SIEMPRE.
    #[test]
    fn sin_rastro_ajeno_se_cae_a_casa() {
        let app = app_remota(&["sftp://srv/a"]);
        assert_eq!(
            destino_tras_desconectar(&app, &vp("sftp://srv/b")),
            norte_frontend::shell::home_vpath(),
        );
    }
}

#[cfg(test)]
mod edit_tests {
    use super::*;
    use crate::app::Pane;

    fn app_local() -> App {
        let d = VPath::parse("file:///tmp").expect("wire de test");
        App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()))
    }

    /// #133: sin nada bajo el cursor no hay nada que editar, y se dice.
    #[test]
    fn editar_la_nada_lo_dice() {
        let app = app_local();
        assert!(edit_under_cursor(&app).is_err());
    }

    /// Una CARPETA no se edita: para entrar está `nav.enter`, y abrirle un
    /// editor a un directorio es enseñarle al editor lo que no sabe.
    #[test]
    fn una_carpeta_no_se_edita() {
        let mut app = app_local();
        app.panes[0].begin_listing(
            VPath::parse("file:///tmp").expect("wire"),
            vec![norte_proto::Entry {
                path: VPath::parse("file:///tmp/sub").expect("wire"),
                kind: norte_proto::EntryKind::Dir,
                size: None,
                mtime_ms: None,
                attrs: std::collections::BTreeMap::new(),
            }],
            false,
            None,
        );
        let err = edit_under_cursor(&app).expect_err("una carpeta no");
        assert!(!err.is_empty());
    }

    /// Un pane REMOTO no tiene fichero de sistema que darle al editor, así que
    /// se dice en vez de abrir nada.
    #[test]
    fn en_un_pane_remoto_no_se_edita() {
        let d = VPath::parse("sftp://host/casa").expect("wire");
        let mut app = App::new(
            Pane::new(d.clone(), Vec::new()),
            Pane::new(d.clone(), Vec::new()),
        );
        app.panes[0].begin_listing(
            d.clone(),
            vec![norte_proto::Entry {
                path: VPath::parse("sftp://host/casa/a.txt").expect("wire"),
                kind: norte_proto::EntryKind::File,
                size: Some(1),
                mtime_ms: None,
                attrs: std::collections::BTreeMap::new(),
            }],
            false,
            None,
        );
        assert!(edit_under_cursor(&app).is_err());
    }

    /// #290: la otra mitad de `pane.edit-new` abre el editor sobre la ruta que
    /// se MANDÓ CREAR, no sobre lo que haya bajo el cursor: cuando la task
    /// termina, el listado puede no haberse refrescado todavía.
    #[test]
    fn el_editor_del_fichero_creado_va_sobre_la_ruta_que_se_pidio() {
        let creado = VPath::parse("file:///tmp/notas.txt").expect("wire");
        let pending = edit_created(&creado).expect("local");
        assert_eq!(pending.argv.len(), 2, "programa y ruta, sin línea de shell");
        assert_eq!(
            pending.argv[1],
            std::ffi::OsString::from("/tmp/notas.txt"),
            "la ruta va como su propio argumento"
        );
        assert!(!pending.wait_for_key, "un editor se despide solo");
        // El cwd es el PADRE del fichero creado, no el pane con foco: entre el
        // submit y el desenlace el lector puede haberse ido a otro sitio.
        assert_eq!(
            pending.cwd.as_deref(),
            Some(std::path::Path::new("/tmp")),
            "un `:w otro.txt` cae donde está el fichero recién creado"
        );
    }

    /// Y sobre algo que no tiene forma nativa no se abre nada: crear se rehúsa
    /// en un pane remoto, así que esto no debería pasar — y si pasa, un editor
    /// sobre lo que no se puede nombrar no es la salida.
    #[test]
    fn sin_forma_nativa_no_se_abre_ningun_editor() {
        let remoto = VPath::parse("sftp://srv/notas.txt").expect("wire");
        assert!(edit_created(&remoto).is_none());
    }

    /// Y sobre un fichero local sale el argv del editor con la ruta APARTE.
    #[test]
    fn sobre_un_fichero_local_sale_el_editor_con_la_ruta_aparte() {
        let mut app = app_local();
        app.panes[0].begin_listing(
            VPath::parse("file:///tmp").expect("wire"),
            vec![norte_proto::Entry {
                path: VPath::parse("file:///tmp/a.txt").expect("wire"),
                kind: norte_proto::EntryKind::File,
                size: Some(1),
                mtime_ms: None,
                attrs: std::collections::BTreeMap::new(),
            }],
            false,
            None,
        );
        let pending = edit_under_cursor(&app).expect("local y fichero");
        assert_eq!(pending.argv.len(), 2, "programa y ruta, sin línea de shell");
        assert_eq!(
            pending.argv[1],
            std::ffi::OsString::from("/tmp/a.txt"),
            "la ruta va como su propio argumento"
        );
        assert!(!pending.wait_for_key, "un editor se despide solo");
    }
}
