//! El overlay de ayuda (H3b): una tecla dentro, y el comando que el bucle de
//! eventos tiene que despachar al salir.
//!
//! Vivía en el root del binario `ntc` —un crate DISTINTO de esta lib—, así que
//! sus 951 líneas de test no podían ser un fichero de `tests/`, que es lo que
//! son.
//!
//! Fichero aparte de [`crate::help`] (los chords que la ayuda PINTA), de
//! [`crate::help_context`] (qué tema abre una pantalla) y de
//! [`crate::help_render`] (el pipeline de render): esto es solo quien lee sus
//! teclas.
//!
//! El rustdoc de [`on_help_key`] estaba PARTIDO en dos por `main.rs`: la
//! introducción y el «TWO REGIMES ...:» que la cierra habían quedado sobre
//! [`HelpDispatch`], y la lista de bullets que ese dos puntos anuncia seguía
//! sobre la función. Aquí vuelven a ser un solo bloque, sin tocar una palabra.

use crossterm::event::{KeyCode, KeyModifiers};
use norte_core::backend::Backend;
use norte_i18n::{t, ta};

use crate::app::{App, HelpOutcome, PAGE, Palette, detail_for_bar, error_message};
use crate::keymap::{Command, Resolution, Resolver, chord_from_crossterm, parse_plugin_key};

/// Las seis teclas que MUEVEN, en la lateral o en el cuerpo.
///
/// Una página, en la lateral, son diez temas; en el cuerpo es la ventana que
/// se ve, que es lo que un lector entiende por «una pantalla» — con diez
/// fijas, `PgDn` bajaba media pantalla en un terminal alto.
fn desplazar(help: &mut crate::app::HelpView, mover: HelpOutcome) {
    let pagina = if help.state.focus() == norte_frontend::help::Focus::Body {
        help.page()
    } else {
        PAGE
    };
    match mover {
        HelpOutcome::Up => help.line_up(),
        HelpOutcome::Down => help.line_down(),
        HelpOutcome::PageUp => help.state.page_up(pagina),
        HelpOutcome::PageDown => help.state.page_down(pagina),
        HelpOutcome::Top => help.state.top(),
        HelpOutcome::Bottom => help.state.bottom(),
        _ => {}
    }
}

/// What [`on_help_key`] hands the run loop to execute.
///
/// Two variants because a help row can name two different KINDS of thing, and
/// only the run loop can run either: this function is sync (its tests are, and
/// its callers are async), while both destinations need an `await`.
///
/// Keeping them apart in the type rather than collapsing to a string is the
/// point — `Command` is the closed, parsed vocabulary of the app (#112), and a
/// plugin key is deliberately NOT in it: its `command_id` half comes from a
/// third-party manifest with no validated charset, so it must never be handed
/// to a lookup as though it were one of ours.
#[derive(Debug, PartialEq, Eq)]
pub enum HelpDispatch {
    /// A built-in command, already parsed against `COMMANDS`.
    Command(Command),
    /// A plugin-contributed command: `(plugin_id, command_id)`, split at the
    /// FIRST colon after the prefix ([`parse_plugin_key`]).
    Plugin(String, String),
}

/// Runs a plugin's command and announces the result (P1, H3e).
///
/// The ONE place either surface dispatches one. It was written inline in the
/// palette's `Enter` arm and the help overlay grew a second need for it in
/// H3e; a copy would have been a second path with its own answer to what a
/// failure looks like, and H3b's rule is that executing from the help goes
/// through the SAME dispatch as the palette, with nothing bypassed.
///
/// Authorisation is the SERVER's: `plugin.run_command` resolves the command
/// against the catalogue and enforces approved+enabled itself
/// (`resolve_runnable`), independently of any snapshot a client froze. What a
/// client-side check buys is agreement with what the reader is looking at, and
/// it is never what permits the call.
///
/// The plugin's output is UNTRUSTED text: it goes through `detail_for_bar`
/// (masked and capped, pattern #73) before it reaches the status bar.
pub async fn run_plugin_command(app: &mut App, backend: &Backend, id: &str, command: &str) {
    app.message = Some(match backend.plugin_run_command(id, command, "").await {
        Ok(output) => ta("msg-plugin-run-ok", &[("output", &detail_for_bar(&output))]),
        Err(e) => error_message(&e),
    });
}

/// Routes one key inside the help overlay (H3b), and answers with the command
/// the run loop must DISPATCH — `Some` only for `Enter` on a runnable body
/// row, and only after this function has already closed the overlay.
///
/// Extracted from the run loop for the same reason as
/// [`on_columns_key`](crate::screens::pickers::on_columns_key):
/// everything here is decidable from `App` plus the `dialog` resolver, and the
/// dispatch it hands back is the one thing that is not.
///
/// TWO REGIMES, the same split the palette and the search dialog already have:
///
/// * While the sidebar filter is open the keys are FIXED. There is no
///   `dialog.*` verb for "type a character", so resolving through the keymap
///   here would make every printable key mean whatever it is bound to instead
///   of itself. `Esc` LEAVES the box keeping the text — the model's contract:
///   leaving a search is not undoing it, `Backspace` is what empties it.
/// * Otherwise the key resolves through the shared `dialog` resolver like
///   every other overlay's, and the resulting command is filtered through
///   [`crate::app::help_action`]/`ALLOW_HELP` — the SAME list the footer
///   hint is generated from. A verb outside it is inert.
///
/// Two keys keep their global meaning ahead of both regimes (H1 T2, as in
/// every other overlay): `ctrl+c` quits, and `ctrl+p` hands what the reader
/// has typed to the command palette — the two are the same model at different
/// speeds (the `help` topic says as much), so the filter should not have to be
/// retyped to cross between them.
///
/// That second bridge is REFUSED while the page covers a modal
/// (`HelpView::over_modal`), the same guard the `Action::Run` arm makes: the
/// palette would open behind a live dialog, painted but unable to receive a
/// key, and every keystroke meant for its filter would be answering the dialog
/// instead.
pub fn on_help_key(
    app: &mut App,
    resolver: &mut Resolver,
    mods: KeyModifiers,
    code: KeyCode,
) -> Option<HelpDispatch> {
    // Salida de emergencia global, hardcodeada ANTES de resolver — como en
    // todos los overlays (la de este fichero, jamás `Command::AppQuit`: no
    // pregunta).
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return None;
    }
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('p') {
        let help = app.help.as_ref()?;
        // Review H3c MAJOR-1: el MISMO guard que el brazo `Action::Run` de
        // abajo, y por una razón peor. Cruzar a la palette dejaría el modal en
        // pie, y la rama de la palette del run loop está gateada por
        // `!modal_wins`: la palette quedaría PINTADA con aspecto de viva y sin
        // recibir una sola tecla — todas caen en la rama del modal y se
        // resuelven contra su allowlist. Teclear `copy` para filtrar sobre una
        // aprobación de agente descartaría `c`, `o`, `p` y la `y` APROBARÍA la
        // mutación. La ayuda se queda abierta y se dice por qué.
        if help.over_modal {
            app.message = Some(t("msg-help-modal-waiting"));
            return None;
        }
        let filter = help.state.filter_raw().to_owned();
        app.help = None;
        // Sin filas de plugin: `Command::AppPalette` las pide al backend y
        // esta función es SÍNCRONA a propósito (todo lo demás aquí lo es).
        // Degradación conocida y acotada — los built-ins, que es lo que la
        // ayuda documenta, están todos.
        let mut palette = Palette::new(crate::palette::rows_for_context(
            &app.palette_rows,
            app.viewer.is_some(),
        ));
        // El filtro CRUDO (`filter_raw`, no el enmascarado para pintar): es
        // lo que se empareja, y la palette lo vuelve a enmascarar al pintarlo.
        for c in filter.chars() {
            palette.push_char(c);
        }
        app.palette = Some(palette);
        return None;
    }
    // Régimen 1: editor de filtro. Teclas FIJAS (ver la doc de arriba).
    if app.help.as_ref()?.state.filtering() {
        // `plain` como en la palette: SHIFT es parte de teclear una mayúscula,
        // no un modificador que cambie el significado de la tecla.
        let plain = mods.is_empty() || mods == KeyModifiers::SHIFT;
        let help = app.help.as_mut()?;
        match code {
            KeyCode::Char(c) if plain => help.state.push_char(c),
            KeyCode::Backspace if plain => help.state.backspace(),
            // Ambas SALEN de la caja conservando el texto: Esc porque el
            // modelo lo promete, Enter porque el filtro ya está aplicado (la
            // lateral se rehace en cada carácter) y lo único que queda por
            // hacer es devolverle las flechas a la navegación.
            KeyCode::Esc | KeyCode::Enter if plain => help.state.end_filter(),
            // Sin salir de la caja: elegir un acierto mientras se sigue
            // afinando la búsqueda es el gesto que hace útil un filtro.
            KeyCode::Up if plain => help.state.up(),
            KeyCode::Down if plain => help.state.down(),
            _ => {}
        }
        return None;
    }

    // Régimen 2: el keymap manda (contexto `dialog`, rebindeable).
    let chord = chord_from_crossterm(mods, code)?;
    let cmd = match resolver.push(chord) {
        Resolution::Run { command: cmd, .. } => cmd,
        // Sin semántica de secuencia definida para overlays (T2), y lo mismo
        // para una tecla ligada a algo que esta build no corre (K1 T4):
        // ignorar y reiniciar el estado de resolución.
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return None;
        }
        Resolution::Reset => return None,
    };
    // La tecla que ABRE la ayuda la CIERRA. `app.help` es un comando de
    // `[global]`, no un verbo de diálogo, así que no vive en `ALLOW_HELP` y
    // sin esta rama F1 sería inerte dentro de la ayuda — la única tecla del
    // teclado que el lector tiene garantizada para este overlay, sin efecto.
    // Se resuelve por el keymap igual que todo lo demás (un rebind de
    // `app.help` mueve las DOS mitades del interruptor a la vez); lo
    // hardcodeado es el significado, no la tecla. Mismo criterio que F9 en
    // `on_theme_picker_key`.
    if cmd == "app.help" {
        app.help = None;
        return None;
    }
    // Fuera de `ALLOW_HELP` la tecla es INERTE (misma disciplina que el resto
    // de overlays: la semántica vive en código, el keymap solo asigna teclas).
    let outcome = crate::app::help_action(&cmd)?;

    let help = app.help.as_mut()?;
    // Leído ANTES del `match`: el brazo que lo consulta ya no tiene `help` a
    // mano (asigna `app.message`, que reclama el préstamo de vuelta).
    let over_modal = help.over_modal;
    match outcome {
        HelpOutcome::TogglePane => help.state.toggle_focus(),
        HelpOutcome::StartFilter => help.state.start_filter(),
        // Con historial, vuelve; SIN historial, cierra. Es lo que convierte
        // `Backspace` en una tecla honesta en vez de una muerta en la raíz:
        // "atrás" desde donde no se puede ir más atrás es salir.
        HelpOutcome::Back => {
            if !help.state.back() {
                app.help = None;
            }
        }
        HelpOutcome::Close => app.help = None,
        HelpOutcome::Activate => match help.state.action().cloned() {
            // Un enlace se sigue y la ayuda SIGUE abierta: leer no es salir.
            Some(norte_frontend::help::Action::Open(id)) => help.state.open(&id),
            Some(norte_frontend::help::Action::Run(cmd)) => {
                // H3c: una ayuda abierta ENCIMA de un modal no despacha nada
                // sobre los panes. `dispatch` planta sus propios modales (una
                // confirmación de copia), así que el comando SUSTITUIRÍA al
                // que está esperando respuesta: una aprobación de agente
                // desaparecería de la pantalla sin que nadie la haya
                // contestado. Se dice y la ayuda se queda abierta — mismo
                // trato que la fila no despachable de abajo.
                if over_modal {
                    app.message = Some(t("msg-help-modal-waiting"));
                    return None;
                }
                // La lista `commands` de un tema puede nombrar un verbo
                // `dialog.*` — el tema `help` documenta tres — y ésos son
                // vocabulario de overlay, no algo que un pane pueda correr:
                // no están en `COMMANDS` y `Command::parse` los rechaza. Se
                // dice y la ayuda se queda abierta; comerse el Enter en
                // silencio se leería como que el comando corrió. (El resto
                // de ids del corpus SÍ parsean: la puerta de documentación
                // los cruza byte a byte contra `COMMANDS ∪ DIALOG_COMMANDS`.)
                // (H3e) Una fila de PLUGIN. Su clave es `plugin:{id}:{cmd}`,
                // que no vive en `COMMANDS` y que `Command::parse` rechaza —
                // así que sin este brazo el Enter caía en el `msg-help-not-
                // runnable` de abajo y la app se negaba a correr justo la fila
                // que ella misma acababa de pintar como disponible, con el pie
                // prometiendo `⏎ ejecutar`. La atenuación era decorativa.
                if let Some((id, command)) = parse_plugin_key(&cmd) {
                    // La foto congelada DIMEA; jamás AUTORIZA. Negarse aquí es
                    // coherencia con lo que el lector tiene delante — una fila
                    // atenuada que al pulsarla corriera sería peor que no
                    // atenuar nada — pero la autoridad sigue siendo
                    // `resolve_runnable` en el servidor, que comprueba
                    // aprobado+activo por su cuenta y no se fía de ningún
                    // cliente. Dos comprobaciones que dicen lo mismo, una
                    // cortés y otra vinculante.
                    if !norte_help::ChordResolver::availability(&*app.help_chords, &cmd)
                        .is_available()
                    {
                        app.message = Some(t("msg-help-not-runnable"));
                        return None;
                    }
                    let (id, command) = (id.to_owned(), command.to_owned());
                    // Cerrar ANTES de despachar, como abajo.
                    app.help = None;
                    return Some(HelpDispatch::Plugin(id, command));
                }
                let Some(parsed) = Command::parse(&cmd) else {
                    // La barra de estado se ve: el overlay ocupa el frame
                    // menos una fila arriba y otra abajo, y la barra es esa
                    // última fila (`ui::help_layout`).
                    app.message = Some(t("msg-help-not-runnable"));
                    return None;
                };
                // Cerrar ANTES de despachar es deliberado: el comando actúa
                // sobre los panes de debajo y la ayuda taparía la
                // confirmación que abra.
                app.help = None;
                return Some(HelpDispatch::Command(parsed));
            }
            // Foco en la lateral. Arrear el cursor ya PREVISUALIZA (abre lo
            // que pisa), así que el tema resaltado suele ser YA el abierto y
            // `open` no haría nada: un Enter mudo, indistinguible de un fallo.
            // Cuando coinciden, Enter entra AL CUERPO; cuando no —el único
            // caso que queda, seguir un `see_also` desde una lista filtrada,
            // donde el resalte se quedó en la fila visible más cercana— abre.
            // En las dos ramas Enter significa lo mismo: «ir a lo que estoy
            // mirando».
            None => {
                let selected = help.state.selected_topic().cloned();
                if selected.is_some_and(|id| id != *help.state.current()) {
                    help.state.open_selected();
                } else {
                    help.state.toggle_focus();
                }
            }
        },
        // Lo que queda son las seis teclas que desplazan (flechas, página,
        // extremos): todas las demás variantes tienen su brazo arriba.
        mover => desplazar(help, mover),
    }
    None
}

/// The help overlay's key routing, driven through [`on_help_key`] — the same
/// seam the run loop uses, so these exercise the WIRING (allowlist, the two
/// regimes, what closes the overlay, what the run loop is asked to dispatch)
/// and not the model underneath, which has its own tests in
/// `norte_frontend::help`.
#[cfg(test)]
mod help_key_tests {
    use super::*;
    use crate::app::{HelpView, Modal, Pane};
    use crate::keymap::{COMMANDS, DIALOG_COMMANDS, Effective, Screen, presets};
    use crate::overlays::{
        close_stale_overlays, help_owns_keys, modal_help_toggle, open_contextual_help,
        refuses_over_modal, settle_help_over_modal,
    };
    use norte_frontend::help::{Focus, SidebarRow};
    use norte_help::{Lang, TopicId};
    use norte_proto::VPath;

    /// An effective of the orthodox preset over the WHOLE vocabulary: the
    /// `dialog` screen merges `[global]` too, so `DIALOG_COMMANDS` alone
    /// would make `build_for` reject the preset outright.
    fn eff(screen: Screen) -> Effective {
        let (_, preset) = presets()
            .into_iter()
            .find(|(n, _)| *n == "orthodox")
            .expect("preset orthodox");
        let known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect();
        Effective::build_for(&preset, &[], &known, screen).expect("efectivo del preset")
    }

    fn dialog_resolver() -> Resolver {
        Resolver::new(eff(Screen::Dialog))
    }

    /// Bajo **vim** el preset liga `app.help` a `f1` Y a `?`. La TUI resuelve
    /// el cierre por el keymap (`cmd == "app.help"` en `on_help_key`), así que
    /// las dos cierran sin que nada las enumere — es la propiedad que la GUI no
    /// tenía y que su `closes_help` le da ahora. El test la pinea aquí para que
    /// un cambio en la resolución del contexto `dialog` no la pierda en
    /// silencio.
    #[test]
    fn bajo_vim_las_dos_teclas_de_ayuda_cierran() {
        use norte_frontend::keymap::Resolution;

        let (_, preset) = presets()
            .into_iter()
            .find(|(n, _)| *n == "vim")
            .expect("preset vim");
        let known: Vec<&str> = COMMANDS
            .iter()
            .copied()
            .chain(DIALOG_COMMANDS.iter().copied())
            .collect();
        let dialog = Effective::build_for(&preset, &[], &known, Screen::Dialog)
            .expect("efectivo dialog del preset vim");

        for (mods, code) in [
            (KeyModifiers::NONE, KeyCode::F(1)),
            (KeyModifiers::NONE, KeyCode::Char('?')),
        ] {
            let mut app = app_with_help();
            let mut resolver = Resolver::new(dialog.clone());
            let chord = chord_from_crossterm(mods, code).expect("chord modelado");
            assert!(
                matches!(resolver.push(chord), Resolution::Run { command: cmd, .. } if cmd == "app.help"),
                "{code:?} es `app.help` en el contexto dialog"
            );
            let mut resolver = Resolver::new(dialog.clone());
            assert!(on_help_key(&mut app, &mut resolver, mods, code).is_none());
            assert!(app.help.is_none(), "{code:?} cierra la ayuda");
        }
    }

    fn app_with_help() -> App {
        let d = VPath::parse("file:///x").expect("wire de test");
        let mut app = App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()));
        app.help = Some(HelpView::new(Lang::En, Vec::new()));
        app
    }

    /// The same app WITHOUT the overlay: the H3c tests open it through the
    /// production seam instead of planting a `HelpView` by hand, because what
    /// they are checking is what that seam decides.
    fn app_with_help_closed() -> App {
        let mut app = app_with_help();
        app.help = None;
        app
    }

    /// `Command::AppHelp`'s whole body ([`open_contextual_help`]), which is
    /// what `F1` runs.
    fn abrir_ayuda(app: &mut App) {
        open_contextual_help(app, Lang::En, &[], None);
    }

    /// The page `context` opens today, or the documented fallback. The corpus
    /// half of the map is data being written page by page (H3h): a test that
    /// hard-coded `copying` here would fail the day a context is claimed and
    /// pass for the wrong reason until then.
    fn pagina_de(context: &str) -> String {
        norte_help::topic_for_context(Lang::En, context)
            .map_or_else(|| "index".to_owned(), |t| t.id.as_str().to_owned())
    }

    fn collision_modal_de_test() -> Modal {
        Modal::Collision {
            retry: crate::tasks::RetrySpec {
                kind: crate::app::TransferKind::Move,
                from: VPath::parse("file:///a").expect("wire de test"),
                to: VPath::parse("file:///b").expect("wire de test"),
                opts: norte_core::TransferOptions::default(),
                name_encoding: None,
            },
        }
    }

    /// Una aprobación de agente: el modal cuyo secuestro por un overlay es el
    /// defecto que `modal_wins` existe para cerrar (H1 MINOR-4).
    fn approval_modal_de_test() -> Modal {
        Modal::ApproveAgentOp {
            req: norte_proto::methods::PolicyApprovalRequired {
                approval_id: 1,
                session: Some("s1".into()),
                op: "copy".into(),
                paths: vec!["mem:///a".into()],
                paths_total: 0,
                ttl_ms: 60_000,
                detail: norte_proto::methods::ApprovalDetail::default(),
            },
        }
    }

    /// El TOFU de una host key: la superficie de SEGURIDAD que una ayuda sí
    /// puede tapar, porque `dialog.trust-host` tiene página (`remote`) y la
    /// aprobación de agente no — sobre ésa `F1` ya no abre nada (MAJOR-2), así
    /// que los tests de «los verbos de debajo son inertes» viven aquí. La
    /// consecuencia es de la misma clase: `dialog.approve` (la `y` del preset)
    /// CONFÍA en una clave sin verificar.
    fn trust_host_modal_de_test() -> Modal {
        Modal::TrustHostKey {
            host: "h".into(),
            port: Some(22),
            algo: "ssh-ed25519".into(),
            fingerprint: "SHA256:AAAA".into(),
            dir: VPath::parse("sftp://h/").expect("wire de test"),
            pane: 0,
            trail: crate::app::Trail::Record,
        }
    }

    /// `F1` desde un pane abre la página del PANE, no el índice, y llega con
    /// el historial vacío: `Esc` cierra el overlay, no camina hacia atrás a un
    /// sitio que el lector no pidió.
    #[test]
    fn f1_abre_la_pagina_del_contexto_y_sin_historial() {
        let mut app = app_with_help_closed();
        abrir_ayuda(&mut app);
        let help = app.help.as_ref().expect("la ayuda se abrió");
        assert_eq!(
            help.state.current().as_str(),
            "panes",
            "el corpus reclama `browse`"
        );
        assert!(!help.over_modal, "no había modal ninguno");

        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Backspace);
        assert!(
            app.help.is_none(),
            "«atrás» en la raíz cierra: la página contextual NO es un paso de navegación"
        );
    }

    /// Review H3c MAJOR-2: sobre un modal SIN página escrita, `F1` no abre
    /// NADA — lo dice y deja la pregunta contestable.
    ///
    /// El fallback al índice vale desde un pane o desde el visor (nadie espera
    /// una decisión), pero sobre un diálogo tapaba una pregunta viva con
    /// «Bienvenido a norte — norte es un gestor de ficheros ortodoxo. Dos
    /// paneles…», congelaba sus verbos, le sustituía el pie y dejaba al lector
    /// caminar del índice a `copying` para leer la prosa de `y`/`n` de OTRO
    /// diálogo mientras la aprobación esperaba detrás. Es la misma decisión que
    /// [`palette_help`] ya había tomado para una fila sin documentar, aplicada
    /// donde importa más.
    #[test]
    fn f1_sobre_un_modal_sin_pagina_no_tapa_la_pregunta() {
        // Se prueba la GUARDA, no el hueco: desde H3h todo contexto tiene
        // página (la puerta de documentación se quedó sin allowlist), así que
        // un test que necesitara un modal indocumentado se quedaría sin sujeto
        // y habría que reescribirlo con cada página nueva. El contexto es
        // sintético; lo que se fija es que sobre un modal la respuesta a «no
        // hay página» es no abrir nada.
        for lang in [Lang::En, Lang::Es] {
            assert!(
                refuses_over_modal(lang, "dialog.no-such-context", true),
                "sobre un modal, sin página, F1 no abre nada"
            );
            assert!(
                !refuses_over_modal(lang, "dialog.no-such-context", false),
                "desde un pane el índice SÍ es un aterrizaje razonable"
            );
            assert!(
                !refuses_over_modal(lang, "dialog.approval", true),
                "y con página escrita se abre esa página"
            );
        }
    }

    /// La otra mitad, y lo que de verdad cambió en H3h: ningún modal que la
    /// TUI sepa abrir se queda sin página. Es lo mismo que cruza la puerta de
    /// documentación (`tests/help_gate.rs`), comprobado aquí desde el lado del
    /// lector — `F1` sobre una pregunta viva abre prosa sobre ESA pregunta, y
    /// nunca el mensaje de arriba.
    #[test]
    fn todo_contexto_de_modal_tiene_pagina() {
        for lang in [Lang::En, Lang::Es] {
            for context in crate::help_context::CONTEXTS {
                assert!(
                    !refuses_over_modal(lang, context, true),
                    "[{lang:?}] el contexto `{context}` no tiene página que abrir"
                );
            }
        }
    }

    /// Y sobre un modal con página, `F1` la abre sin tapar la pregunta.
    #[test]
    fn f1_sobre_una_aprobacion_abre_su_pagina() {
        let mut app = app_with_help_closed();
        app.modal = Some(approval_modal_de_test());
        abrir_ayuda(&mut app);
        let help = app.help.as_ref().expect("la ayuda se abrió");
        assert_eq!(help.state.current().as_str(), "agents");
        assert!(help.over_modal, "la ayuda sabe que hay una pregunta detrás");
        assert!(app.modal.is_some(), "y la pregunta sigue ahí");
    }

    /// `F1` sobre un modal abre la página de ESE modal (o el índice mientras
    /// nadie la haya escrito) y deja el modal donde estaba.
    #[test]
    fn f1_sobre_un_modal_abre_la_pagina_del_modal() {
        let mut app = app_with_help_closed();
        app.modal = Some(collision_modal_de_test());
        abrir_ayuda(&mut app);
        let help = app.help.as_ref().expect("la ayuda se abrió");
        assert_eq!(
            help.state.current().as_str(),
            pagina_de("dialog.collision"),
            "la página del contexto del modal, jamás la de otro modal"
        );
        assert!(help.over_modal, "se abrió ENCIMA de un modal");
        assert!(app.modal.is_some(), "y el modal sigue ahí");
        assert!(
            help_owns_keys(&app),
            "…con las teclas: sin esto la rama del modal se las quedaría y el \
             lector no podría ni mover el cursor de la ayuda que acaba de abrir"
        );
    }

    /// La ayuda abierta desde un modal se queda las teclas, y `Esc` cierra
    /// SOLO la ayuda: el modal no se responde por accidente.
    #[test]
    fn esc_cierra_la_ayuda_y_deja_el_modal_intacto() {
        let mut app = app_with_help_closed();
        app.modal = Some(trust_host_modal_de_test());
        abrir_ayuda(&mut app);
        assert!(help_owns_keys(&app), "la rama de la ayuda es la que corre");
        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Esc);
        assert!(app.help.is_none(), "la ayuda se cerró");
        assert!(
            app.modal.is_some(),
            "una host key desconocida NO se contesta cerrando una ayuda"
        );
        assert!(
            !help_owns_keys(&app),
            "y cerrada, la tecla siguiente vuelve al modal"
        );
    }

    /// Mientras la ayuda tapa el modal, los verbos del modal son inertes: se
    /// decide con la ayuda cerrada, mirándolo.
    #[test]
    fn los_verbos_del_modal_no_se_alcanzan_por_debajo_de_la_ayuda() {
        let mut app = app_with_help_closed();
        app.modal = Some(trust_host_modal_de_test());
        abrir_ayuda(&mut app);
        // La rama que corre es la de la ayuda (`help_owns_keys`), así que la
        // del modal —la única que llama a `dialog_action`— no ve esta tecla.
        assert!(help_owns_keys(&app));
        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Char('y')); // dialog.approve
        assert!(app.modal.is_some(), "no se confió en nada a ciegas");
        assert!(app.help.is_some(), "y `y` tampoco cierra la ayuda");
    }

    /// Un modal que LLEGA sobre una ayuda abierta la cierra: la tecla
    /// siguiente tiene que ir donde apuntan los píxeles (el modal se pinta
    /// ÚLTIMO, por encima de todo), y una aprobación no se contesta a través
    /// de una página.
    #[test]
    fn un_modal_que_llega_cierra_la_ayuda() {
        let mut app = app_with_help_closed();
        abrir_ayuda(&mut app); // sin modal: over_modal == false
        assert!(!app.help.as_ref().expect("abierta").over_modal);
        app.modal = Some(approval_modal_de_test());
        assert!(
            !help_owns_keys(&app),
            "la ayuda que ya estaba abierta NO se queda la tecla del modal"
        );
        close_stale_overlays(&mut app);
        assert!(app.help.is_none(), "la ayuda cede la pantalla");
    }

    /// Review MINOR-1: `over_modal` es un hecho del PRESENTE, no un recuerdo.
    ///
    /// Si el modal sobre el que se abrió la ayuda desaparece y llega OTRO, la
    /// bandera vieja haría que la ayuda se quedara las teclas y
    /// `close_stale_overlays` no la retirara nunca: el modal nuevo sería
    /// incontestable hasta cerrar una página sobre un diálogo que ya no existe.
    /// `settle_help_over_modal` la limpia en cuanto no hay modal, así que el
    /// segundo modal se trata como lo que es — uno que LLEGA sobre una ayuda
    /// abierta.
    #[test]
    fn over_modal_no_sobrevive_al_modal_que_lo_justificaba() {
        let mut app = app_with_help_closed();
        app.modal = Some(collision_modal_de_test());
        abrir_ayuda(&mut app);
        assert!(app.help.as_ref().expect("abierta").over_modal);

        // El modal se contesta; la ayuda sigue abierta (Esc cerraría solo la
        // ayuda, pero el modal puede irse por su propio camino: un retry).
        app.modal = None;
        settle_help_over_modal(&mut app);
        assert!(
            !app.help.as_ref().expect("sigue abierta").over_modal,
            "la bandera no sobrevive a lo que era un recuerdo DE"
        );

        // …y ahora llega otro modal, que NO hereda las teclas de la ayuda.
        app.modal = Some(approval_modal_de_test());
        assert!(
            !help_owns_keys(&app),
            "el modal nuevo se queda la tecla: nadie pidió una página sobre ÉL"
        );
        close_stale_overlays(&mut app);
        assert!(app.help.is_none(), "y la ayuda caduca como el resto");
    }

    /// Y la ayuda que tapa un modal tampoco DESPACHA: `dispatch` planta sus
    /// propios modales, así que correr `pane.copy` desde la página sustituiría
    /// la pregunta que espera respuesta — desaparecería de la pantalla sin que
    /// nadie la haya contestado. Se dice y la página se queda.
    #[test]
    fn una_fila_ejecutable_no_se_despacha_por_encima_de_un_modal() {
        let mut app = app_with_help_closed();
        app.modal = Some(trust_host_modal_de_test());
        abrir_ayuda(&mut app);
        let mut r = dialog_resolver();
        // A una página con filas ejecutables (la del contexto puede no
        // tenerlas todavía) y al cuerpo, que es donde vive el Enter.
        app.help
            .as_mut()
            .expect("abierta")
            .state
            .open(&TopicId::new("copying"));
        press(&mut app, &mut r, KeyCode::Tab);
        assert_eq!(state(&app).focus(), Focus::Body);
        assert!(matches!(
            state(&app).action(),
            Some(norte_frontend::help::Action::Run(_))
        ));

        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(cmd, None, "nada que el run loop pueda despachar");
        assert!(app.help.is_some(), "y la ayuda no se cierra sola");
        assert!(app.modal.is_some(), "la pregunta sigue en pie");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-help-modal-waiting").as_str()),
            "el Enter no puede desaparecer en silencio"
        );
    }

    /// Y el PUENTE a la palette tampoco cruza por encima de un modal (review
    /// H3c MAJOR-1), por la misma razón que el brazo de arriba y con una
    /// consecuencia peor.
    ///
    /// `Ctrl+P` cerraba la ayuda y abría la palette dejando el modal en pie.
    /// La rama de la palette del run loop está gateada por `!modal_wins`, así
    /// que la palette quedaba PINTADA y con aspecto de viva pero sin recibir
    /// una sola tecla: todas caían en la rama del modal y se resolvían contra
    /// su allowlist. Teclear `copy` para filtrar sobre este TOFU descarta `c`,
    /// `o`, `p`… y la `y` CONFÍA en la host key.
    #[test]
    fn el_puente_a_la_palette_no_cruza_por_encima_de_un_modal() {
        let mut app = app_with_help_closed();
        app.modal = Some(trust_host_modal_de_test());
        abrir_ayuda(&mut app);
        assert!(
            app.help.as_ref().expect("abierta").over_modal,
            "precondición: la ayuda se abrió ENCIMA del modal"
        );
        let mut r = dialog_resolver();

        let cmd = on_help_key(&mut app, &mut r, KeyModifiers::CONTROL, KeyCode::Char('p'));
        assert_eq!(cmd, None, "nada que despachar");
        assert!(
            app.palette.is_none(),
            "la palette NO se abre: sus teclas se las quedaría el modal"
        );
        assert!(app.help.is_some(), "la ayuda se queda donde estaba");
        assert!(app.modal.is_some(), "y el modal sigue esperando respuesta");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-help-modal-waiting").as_str()),
            "el Ctrl+P no puede desaparecer en silencio"
        );
    }

    /// La tecla de la ayuda tiene que LLEGAR con un modal abierto. `app.help`
    /// es un comando de `[global]`, no un verbo `dialog.*`, así que el
    /// allowlist del modal (`dialog_action`) lo deja caer: sin la rama de
    /// `modal_help_toggle` en `on_dialog_key`, `F1` sobre un diálogo es INERTE
    /// y toda esta tarea no se puede usar. (Pillado pilotando la TUI en tmux:
    /// la suite en verde no lo veía porque abría la ayuda por `dispatch`.)
    #[test]
    fn f1_resuelve_y_abre_la_ayuda_con_un_modal_abierto() {
        let mut app = app_with_help_closed();
        app.modal = Some(collision_modal_de_test());
        let mut r = dialog_resolver();

        // El MISMO camino que el run loop: el chord de F1 resuelto contra el
        // efectivo `dialog` — la tecla es del keymap (rebindeable), el
        // significado es de aquí.
        let chord =
            chord_from_crossterm(KeyModifiers::NONE, KeyCode::F(1)).expect("F1 es un chord");
        let cmd = match r.push(chord) {
            Resolution::Run { command: cmd, .. } => cmd,
            otro => panic!("F1 resuelve a un comando en el contexto dialog: {otro:?}"),
        };
        assert_eq!(cmd, "app.help", "el preset orthodox ata F1 a `app.help`");

        assert!(
            modal_help_toggle(&mut app, &cmd, Lang::En, &[]),
            "la tecla se CONSUME: el modal no la ve como una decisión"
        );
        let help = app.help.as_ref().expect("F1 abrió la ayuda sobre el modal");
        assert_eq!(help.state.current().as_str(), pagina_de("dialog.collision"));
        assert!(help.over_modal);
        assert!(app.modal.is_some(), "y el modal sigue en pie");

        // Y con la ayuda ya abierta la MISMA tecla la cierra (el interruptor
        // vive en `on_help_key`), así que este hook no puede reabrirla: la
        // rama de la ayuda gana la tecla antes de llegar aquí.
        assert!(help_owns_keys(&app));
    }

    /// Cualquier otro comando `dialog.*` no lo toca el hook: quien decide
    /// sigue siendo el allowlist del modal.
    #[test]
    fn el_hook_de_la_ayuda_no_se_come_los_verbos_del_modal() {
        let mut app = app_with_help_closed();
        app.modal = Some(approval_modal_de_test());
        assert!(!modal_help_toggle(
            &mut app,
            "dialog.approve",
            Lang::En,
            &[]
        ));
        assert!(app.help.is_none(), "ni abre nada");
    }

    /// Review H3c MINOR-3: los SEIS modales que el run loop intercepta antes de
    /// `on_dialog_key` no admiten ayuda por encima, y ahora eso es una DECISIÓN
    /// (`help_context::help_over_modal_allowed`) en vez de la resaca del
    /// enrutado de teclas.
    ///
    /// Antes quedaban fuera solo porque cada uno hace `continue` 3000 líneas
    /// más arriba; mover uno al keymap `dialog` —una limpieza plausible— habría
    /// abierto el agujero en silencio sobre un editor de texto libre y sobre el
    /// TOFU de `init.lua`, que NO tiene TTL.
    #[test]
    fn los_modales_interceptados_no_admiten_ayuda_por_encima() {
        let intercepted = [
            Modal::TrustLuaInit {
                path: "repo/.norte/init.lua".into(),
                hash_abbrev: "ab12cd34ef56ab78ab12cd34ef56ab78".into(),
            },
            Modal::MarkPattern {
                mark: true,
                pattern: "*.rs".into(),
                error: None,
            },
            Modal::Mkdir {
                name: "nuevo".into(),
                error: None,
            },
            Modal::CommandLine {
                command: "make test".into(),
                error: None,
            },
            Modal::AiRenameInstruction {
                instruction: "en snake_case".into(),
                error: None,
            },
            Modal::SemanticQuery {
                query: "facturas".into(),
                error: None,
            },
            Modal::TransferName {
                kind: crate::app::TransferKind::Copy,
                from: VPath::parse("file:///x/a").expect("wire de test"),
                to_dir: VPath::parse("file:///y").expect("wire de test"),
                name: "a".into(),
                original: b"a".to_vec(),
                touched: false,
                from_marks: false,
                enc: None,
                error: None,
                space: None,
                confine: None,
            },
        ];
        for modal in intercepted {
            let label = format!("{modal:?}");
            let mut app = app_with_help_closed();
            app.modal = Some(modal);
            assert!(
                !modal_help_toggle(&mut app, "app.help", Lang::En, &[]),
                "{label}: el hook no puede CONSUMIR la tecla de un modal que \
                 no admite ayuda — quien decide vuelve a ser el allowlist"
            );
            assert!(
                app.help.is_none(),
                "{label}: F1 no abre una página sobre un editor de texto \
                 libre ni sobre el TOFU de Lua"
            );
            assert!(app.modal.is_some(), "{label}: y el modal sigue ahí");
        }
    }

    /// …y la dirección contraria NO: una ayuda que el lector abrió DESDE el
    /// modal sobrevive a la limpieza, o `F1` sobre un diálogo abriría una
    /// página que la siguiente tecla se lleva.
    #[test]
    fn la_ayuda_abierta_desde_el_modal_sobrevive_a_la_limpieza() {
        let mut app = app_with_help_closed();
        app.modal = Some(trust_host_modal_de_test());
        abrir_ayuda(&mut app);
        app.palette = Some(Palette::new(Vec::new()));
        close_stale_overlays(&mut app);
        assert!(app.palette.is_none(), "la palette sí caduca");
        assert!(
            app.help.is_some(),
            "la ayuda que el lector pidió sobre ESTE modal se queda"
        );
    }

    /// One unmodified key press.
    fn press(app: &mut App, resolver: &mut Resolver, code: KeyCode) -> Option<HelpDispatch> {
        on_help_key(app, resolver, KeyModifiers::NONE, code)
    }

    fn state(app: &App) -> &norte_frontend::help::HelpState {
        &app.help.as_ref().expect("overlay abierto").state
    }

    fn topic_ids(app: &App) -> Vec<String> {
        state(app)
            .rows()
            .iter()
            .filter_map(|r| match r {
                SidebarRow::Topic { id, .. } => Some(id.as_str().to_owned()),
                SidebarRow::Group { .. } => None,
            })
            .collect()
    }

    /// `/` opens the filter, the characters narrow the sidebar, and `Esc`
    /// leaves the box KEEPING what was typed — the model's contract, and the
    /// reason the filter is not a modal editor: leaving a search is not
    /// undoing it.
    #[test]
    fn the_filter_editor_types_narrows_and_keeps_its_text_on_esc() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        let todos = topic_ids(&app);
        assert!(todos.len() > 3, "el corpus trae varias páginas: {todos:?}");

        press(&mut app, &mut r, KeyCode::Char('/'));
        assert!(state(&app).filtering(), "`/` abre el filtro");

        for c in "copying".chars() {
            press(&mut app, &mut r, KeyCode::Char(c));
        }
        let filtered = topic_ids(&app);
        assert_eq!(
            filtered,
            vec!["copying".to_owned()],
            "la lateral se estrecha a lo tecleado"
        );
        assert!(
            filtered.len() < todos.len(),
            "el filtro tiene que quitar algo o no filtra nada"
        );

        // Y las teclas son FIJAS: `/` es un carácter más dentro de la caja, no
        // el verbo `dialog.filter` otra vez.
        press(&mut app, &mut r, KeyCode::Char('/'));
        assert_eq!(state(&app).filter_raw(), "copying/");
        press(&mut app, &mut r, KeyCode::Backspace);
        assert_eq!(state(&app).filter_raw(), "copying");

        press(&mut app, &mut r, KeyCode::Esc);
        assert!(!state(&app).filtering(), "Esc sale de la caja");
        assert_eq!(
            state(&app).filter_raw(),
            "copying",
            "…CONSERVANDO el texto: salir de una búsqueda no es deshacerla"
        );
        assert!(app.help.is_some(), "y Esc en la caja NO cierra el overlay");
    }

    /// Enter sobre una fila `Action::Run` devuelve el comando que el run loop
    /// debe despachar — el MISMO id que mandaría la palette — y deja el
    /// overlay CERRADO: el comando actúa sobre los panes de debajo.
    #[test]
    fn enter_on_a_runnable_row_hands_the_command_over_and_closes() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        app.help
            .as_mut()
            .expect("abierto")
            .state
            .open(&TopicId::new("copying"));
        press(&mut app, &mut r, KeyCode::Tab);
        assert_eq!(state(&app).focus(), Focus::Body, "Tab pasa al cuerpo");

        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(
            cmd,
            Some(HelpDispatch::Command(Command::PaneCopy)),
            "la primera fila de `copying` es `pane.copy`"
        );
        assert!(
            app.help.is_none(),
            "el overlay se cierra ANTES de despachar"
        );
    }

    /// Deja la ayuda abierta sobre la página de `acme.ftp`, con una fila
    /// ejecutable y el foco ya en el cuerpo: lo que ve un lector que llegó por
    /// `F1` desde el gestor de extensiones.
    fn app_con_pagina_de_plugin(activo: bool) -> (App, Resolver) {
        let mut app = app_with_help();
        let mut plugin = norte_proto::methods::PluginInfo {
            id: "acme.ftp".into(),
            name: "FTP".into(),
            publisher: "ACME".into(),
            version: "1.0.0".into(),
            category: "command".into(),
            capabilities: Vec::new(),
            approved: activo,
            enabled: activo,
            description: None,
            commands: vec![norte_proto::methods::PluginCommandInfo {
                id: "sync".into(),
                title: "Sincronizar".into(),
                kind: norte_proto::methods::PluginCommandKind::Command,
            }],
            columns: Vec::new(),
            panels: Vec::new(),
            has_help: true,
            manifest_digest: None,
        };
        plugin.has_help = true;
        app.freeze_help_plugins(std::slice::from_ref(&plugin));
        let help = app.help.as_mut().expect("abierto");
        help.state.open(&TopicId::new("acme.ftp"));
        let parsed = norte_help::parse_untrusted(
            b"+++\nid = \"acme.ftp\"\ntitle = \"FTP\"\n\
              commands = [\"plugin:acme.ftp:sync\"]\n+++\ncuerpo",
            "acme.ftp",
            None,
        );
        help.state.install_plugin_topic(parsed.topic);
        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Tab);
        assert_eq!(state(&app).focus(), Focus::Body, "Tab pasa al cuerpo");
        (app, r)
    }

    /// H3e: Enter sobre la fila de un plugin ACTIVO la despacha de verdad.
    ///
    /// No lo hacía. La clave es `plugin:{id}:{cmd}`, que no vive en `COMMANDS`
    /// y que `Command::parse` rechaza, así que el Enter caía en el brazo de
    /// «esta fila no es ejecutable» — sobre una fila que el propio resolver
    /// acababa de pintar como DISPONIBLE, con el pie prometiendo `⏎ ejecutar`.
    /// La atenuación de `verdict_with_plugins` era decorativa: la app se negaba
    /// tanto con la fila encendida como con la apagada.
    #[test]
    fn enter_sobre_la_fila_de_un_plugin_activo_la_despacha() {
        let (mut app, mut r) = app_con_pagina_de_plugin(true);
        assert!(
            norte_help::ChordResolver::availability(&*app.help_chords, "plugin:acme.ftp:sync")
                .is_available(),
            "la premisa: el resolver la pinta disponible"
        );
        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(
            cmd,
            Some(HelpDispatch::Plugin(
                "acme.ftp".to_owned(),
                "sync".to_owned()
            )),
            "el run loop recibe qué plugin y qué comando, ya separados"
        );
        assert!(
            app.help.is_none(),
            "y el overlay se cierra ANTES de despachar, como con un built-in"
        );
    }

    /// Y sobre la de un plugin APAGADO se niega. La foto congelada no autoriza
    /// nada —`plugin.run_command` comprueba aprobado+activo por su cuenta en el
    /// servidor— pero una fila atenuada que al pulsarla corriera sería peor que
    /// no atenuar nada: el lector aprendería que la atenuación no significa
    /// nada.
    #[test]
    fn enter_sobre_la_fila_de_un_plugin_apagado_se_niega() {
        let (mut app, mut r) = app_con_pagina_de_plugin(false);
        assert_eq!(
            norte_help::ChordResolver::availability(&*app.help_chords, "plugin:acme.ftp:sync")
                .reason(),
            Some(norte_help::Reason::PluginInactive),
            "la premisa: el resolver la pinta atenuada"
        );
        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(cmd, None, "no se despacha nada");
        assert!(app.help.is_some(), "y la ayuda se queda abierta");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-help-not-runnable").as_str()),
            "comerse el Enter en silencio se leería como que el comando corrió"
        );
    }

    /// La lista `commands` de un tema puede nombrar un verbo `dialog.*` (el
    /// tema `help` documenta tres): no son despachables desde un pane. No se
    /// despacha nada, el overlay SIGUE abierto y se dice — comerse el Enter
    /// en silencio se leería como que el comando corrió.
    #[test]
    fn enter_on_a_dialog_verb_row_dispatches_nothing_and_says_so() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        app.help
            .as_mut()
            .expect("abierto")
            .state
            .open(&TopicId::new("help"));
        press(&mut app, &mut r, KeyCode::Tab);
        // `commands` del tema `help`: app.help, app.palette, dialog.filter…
        press(&mut app, &mut r, KeyCode::Down);
        press(&mut app, &mut r, KeyCode::Down);
        assert_eq!(
            state(&app).action(),
            Some(&norte_frontend::help::Action::Run("dialog.filter".into())),
            "la tercera fila del tema `help` es un verbo de overlay"
        );

        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(cmd, None, "un `dialog.*` no se despacha desde un pane");
        assert!(app.help.is_some(), "y el overlay se queda donde estaba");
        assert_eq!(
            app.message.as_deref(),
            Some(norte_i18n::t("msg-help-not-runnable").as_str()),
            "el Enter no puede desaparecer en silencio"
        );
    }

    /// Enter sobre un enlace lo SIGUE y el overlay sigue abierto (leer no es
    /// salir); `dialog.back` vuelve a la página de la que venía.
    #[test]
    fn enter_on_a_link_follows_it_and_back_returns() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        assert_eq!(state(&app).current().as_str(), "index");
        // El índice no tiene `commands`: todas sus acciones son `see_also`.
        press(&mut app, &mut r, KeyCode::Tab);
        assert_eq!(state(&app).focus(), Focus::Body);
        let dest = match state(&app).action() {
            Some(norte_frontend::help::Action::Open(id)) => id.as_str().to_owned(),
            otro => panic!("la primera acción del índice es un enlace: {otro:?}"),
        };

        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(cmd, None, "un enlace no despacha nada");
        assert!(app.help.is_some(), "…y el overlay SIGUE abierto");
        assert_eq!(state(&app).current().as_str(), dest);

        press(&mut app, &mut r, KeyCode::Backspace);
        assert!(app.help.is_some(), "volver tampoco cierra");
        assert_eq!(state(&app).current().as_str(), "index");
    }

    /// Enter en la lateral SOBRE EL TEMA YA ABIERTO entra al cuerpo. Arrear la
    /// lateral previsualiza, así que ése es el caso normal y `open` sería un
    /// no-op: un Enter mudo que nadie puede distinguir de un fallo.
    #[test]
    fn enter_on_the_open_topic_moves_the_focus_into_the_body() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        assert_eq!(state(&app).focus(), Focus::Topics);
        assert_eq!(
            state(&app).selected_topic().map(TopicId::as_str),
            Some(state(&app).current().as_str()),
            "el cursor de la lateral se apoya en el tema abierto"
        );

        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(cmd, None, "entrar al cuerpo no despacha nada");
        assert!(app.help.is_some(), "…ni cierra el overlay");
        assert_eq!(
            state(&app).focus(),
            Focus::Body,
            "Enter significa «ir a lo que estoy mirando»"
        );
    }

    /// La otra rama: con el resalte sobre un tema DISTINTO del abierto —lo
    /// que pasa al seguir un `see_also` desde una lista filtrada, donde el
    /// resalte se queda en la fila visible más cercana— Enter lo abre.
    #[test]
    fn enter_on_a_topic_that_is_not_the_open_one_opens_it() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        // Filtrar a `copying` y seguir su primer enlace: el destino no está
        // en la lateral filtrada, así que el resalte se queda en `copying`.
        press(&mut app, &mut r, KeyCode::Char('/'));
        for c in "copying".chars() {
            press(&mut app, &mut r, KeyCode::Char(c));
        }
        press(&mut app, &mut r, KeyCode::Esc);
        assert_eq!(topic_ids(&app), vec!["copying".to_owned()]);
        press(&mut app, &mut r, KeyCode::Tab);
        assert_eq!(state(&app).focus(), Focus::Body);
        while !matches!(
            state(&app).action(),
            Some(norte_frontend::help::Action::Open(_))
        ) {
            press(&mut app, &mut r, KeyCode::Down);
        }
        press(&mut app, &mut r, KeyCode::Enter);
        let open = state(&app).current().as_str().to_owned();
        assert_ne!(open, "copying", "el enlace llevó a otra página");
        assert_eq!(
            state(&app).selected_topic().map(TopicId::as_str),
            Some("copying"),
            "…y el resalte se quedó donde el filtro lo dejó"
        );

        // Enter en la lateral abre lo resaltado, que NO es lo abierto.
        press(&mut app, &mut r, KeyCode::Tab);
        assert_eq!(state(&app).focus(), Focus::Topics);
        let cmd = press(&mut app, &mut r, KeyCode::Enter);
        assert_eq!(cmd, None);
        assert_eq!(
            state(&app).current().as_str(),
            "copying",
            "Enter abre el tema resaltado"
        );
    }

    /// `dialog.back` en la RAÍZ (sin historial) cierra el overlay. Es lo que
    /// convierte `Backspace` en una tecla honesta en vez de una muerta.
    #[test]
    fn back_at_the_root_closes_the_overlay() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Backspace);
        assert!(
            app.help.is_none(),
            "sin historial, «atrás» solo puede significar salir"
        );
    }

    /// Un verbo `dialog.*` FUERA de `ALLOW_HELP` es INERTE aquí, aunque el
    /// keymap lo tenga bien atado: la semántica de cada overlay vive en
    /// código. `y` es `dialog.approve` en el preset orthodox.
    #[test]
    fn a_verb_outside_the_allowlist_is_inert() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        let before = state(&app).current().clone();
        let cmd = press(&mut app, &mut r, KeyCode::Char('y'));
        assert_eq!(cmd, None);
        assert!(app.help.is_some(), "`dialog.approve` no cierra la ayuda");
        assert_eq!(state(&app).current(), &before, "ni navega");
    }

    /// La tecla que abre la ayuda la cierra: F1 resuelve a `app.help`, que
    /// NO está en `ALLOW_HELP` (es de `[global]`), y sin su rama propia sería
    /// inerte justo dentro del overlay que abre.
    #[test]
    fn the_key_that_opens_the_help_closes_it() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        let cmd = press(&mut app, &mut r, KeyCode::F(1));
        assert_eq!(cmd, None, "cerrar no despacha nada");
        assert!(app.help.is_none(), "F1 dentro de la ayuda la cierra");
    }

    /// …pero no mientras se teclea en el filtro: ahí la caja consume la
    /// tecla, como en la palette y el diálogo de búsqueda.
    #[test]
    fn the_filter_box_keeps_the_toggle_key() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Char('/'));
        assert!(state(&app).filtering());
        press(&mut app, &mut r, KeyCode::F(1));
        assert!(
            app.help.is_some(),
            "una tecla de función dentro del editor no cierra el overlay"
        );
    }

    /// `ctrl+c` conserva su salida global y `ctrl+p` cruza a la palette
    /// LLEVÁNDOSE el filtro — los dos son el mismo modelo a dos velocidades
    /// (lo dice el tema `help`), así que no hay que reteclearlo.
    #[test]
    fn ctrl_c_quits_and_ctrl_p_hands_the_filter_to_the_palette() {
        let mut app = app_with_help();
        let mut r = dialog_resolver();
        on_help_key(&mut app, &mut r, KeyModifiers::CONTROL, KeyCode::Char('c'));
        assert!(app.quit, "la salida de emergencia va antes que todo");

        let mut app = app_with_help();
        app.palette_rows = crate::palette::build_rows(&eff(Screen::Browse), &eff(Screen::Viewer));
        press(&mut app, &mut r, KeyCode::Char('/'));
        for c in "copy".chars() {
            press(&mut app, &mut r, KeyCode::Char(c));
        }
        on_help_key(&mut app, &mut r, KeyModifiers::CONTROL, KeyCode::Char('p'));
        assert!(app.help.is_none(), "la ayuda cede el sitio");
        let palette = app.palette.as_ref().expect("la palette abrió");
        assert!(
            !palette.visible().is_empty(),
            "el filtro llegó y sigue casando algo"
        );
        assert!(
            palette.visible().len() < palette.rows().len(),
            "…y de verdad filtró: {} de {}",
            palette.visible().len(),
            palette.rows().len()
        );
    }
}
