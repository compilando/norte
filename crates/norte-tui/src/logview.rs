//! El panel de registro en la TUI: el kind y las teclas.
//!
//! El estado (nivel, filtro, seguimiento del final) vive en
//! [`norte_frontend::logpanel`] porque la ventana necesita el mismo, y las
//! líneas vienen del anillo de `norte_config::logring`. Aquí queda lo que es de
//! esta terminal: qué tecla hace qué.

/// El kind que ocupa un hueco de registro.
pub const KIND: &str = "log";

/// Cuántas filas avanza una página.
///
/// El ALTO ya no se adivina aquí —lo pone quien pinta, por frame
/// (`LogPanel::set_viewport_rows`)—; esto es solo cuánto salta `AvPág`.
const PAGINA: isize = 10;

/// Lo que una tecla le pide al panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogAction {
    /// Enseñar hasta este nivel.
    Level(norte_config::logline::LogLevel),
    /// Subir o bajar `n` líneas.
    Scroll(isize),
    /// Volver a pegarse al final.
    Follow,
    /// Empezar a teclear un filtro.
    StartFilter,
    /// Devolver el teclado.
    Leave,
}

/// Traduce una tecla del panel de registro.
///
/// Un `match` explícito y NO el keymap: estas teclas solo existen mientras el
/// panel tiene el teclado, son de una sola letra, y meterlas en el keymap
/// obligaría a los siete presets a declarar cinco atajos que fuera de aquí no
/// significan nada. Es el mismo criterio que el selector de conexiones y el de
/// disposición.
#[must_use]
pub fn key(
    code: crossterm::event::KeyCode,
    mods: crossterm::event::KeyModifiers,
) -> Option<LogAction> {
    use crossterm::event::{KeyCode, KeyModifiers};
    use norte_config::logline::LogLevel;
    if !(mods.is_empty() || mods == KeyModifiers::SHIFT) {
        return None;
    }
    Some(match code {
        KeyCode::Char('e') => LogAction::Level(LogLevel::Error),
        KeyCode::Char('w') => LogAction::Level(LogLevel::Warn),
        KeyCode::Char('i') => LogAction::Level(LogLevel::Info),
        KeyCode::Char('d') => LogAction::Level(LogLevel::Debug),
        KeyCode::Char('t') => LogAction::Level(LogLevel::Trace),
        KeyCode::Char('/') => LogAction::StartFilter,
        KeyCode::Up => LogAction::Scroll(-1),
        KeyCode::Down => LogAction::Scroll(1),
        KeyCode::PageUp => LogAction::Scroll(-PAGINA),
        KeyCode::PageDown => LogAction::Scroll(PAGINA),
        // `End` es «vuelve a lo último», que es distinto de bajar mucho: tras
        // un filtro nuevo la lista cambia de largo y bajar a ciegas no acierta.
        KeyCode::End => LogAction::Follow,
        // Y `Inicio`, al principio de lo que quede: quien tiene `Fin` lo busca.
        // `isize::MIN` no, que se desbordaría al negarlo — el desplazamiento se
        // acota solo contra el tope.
        KeyCode::Home => LogAction::Scroll(isize::MIN + 1),
        KeyCode::Esc => LogAction::Leave,
        _ => return None,
    })
}

/// Aplica una tecla al panel de registro de `app`.
///
/// Subir el nivel del PANEL sube también el del ANILLO cuando hace falta: sin
/// eso, pedir DEBUG filtraría a DEBUG unas líneas que se guardaron a INFO, o
/// sea que enseñaría exactamente nada y parecería roto. Bajarlo no baja el del
/// anillo — ver la nota de [`norte_frontend::logpanel`].
pub fn apply(
    app: &mut crate::app::App,
    resolver: &mut crate::keymap::Resolver,
    mods: crossterm::event::KeyModifiers,
    code: crossterm::event::KeyCode,
) {
    use crossterm::event::{KeyCode, KeyModifiers};
    // `Ctrl+C` ANTES que nada, también con el campo de filtro abierto: es la
    // salida de emergencia, y todos los demás manejadores de este árbol la
    // comprueban primero. Estaba después y escribir un filtro dejaba al lector
    // sin forma de salir del programa.
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    // Con el campo de filtro abierto, el resto de teclas son suyas: si no, una
    // `d` a mitad de una palabra cambiaría el nivel en vez de escribirse.
    if app.log_filter_input.is_some() {
        editar_filtro(app, mods, code);
        return;
    }
    let Some(accion) = key(code, mods) else {
        // Lo que este panel NO es suyo sigue su camino por el keymap, y esto
        // no es un detalle: sin ello el propio `layout.log` moría aquí y el
        // panel no se podía cerrar con la misma tecla que lo abrió. Un panel
        // que se queda TODAS las teclas secuestra el teclado en vez de
        // tomarlo.
        pasar_al_keymap(app, resolver, mods, code);
        return;
    };
    aplicar_accion(app, accion);
}

/// Lo que hace cada acción del panel.
///
/// Separado de [`apply`] para que se pueda probar sin montar un resolver de
/// teclas: lo que estas líneas deciden —cuándo sube el nivel del anillo y
/// cuándo no— es el invariante del panel, no la traducción de una tecla.
pub fn aplicar_accion(app: &mut crate::app::App, accion: LogAction) {
    match accion {
        LogAction::Level(l) => {
            app.log_panel.show_level(l);
            // Y el anillo captura AL MENOS eso: filtrar a DEBUG lo que se
            // guardó a INFO no enseñaría nada y parecería roto. `raise_to`
            // nunca baja — ver su rustdoc.
            if let Some(ring) = app.log_ring.as_ref() {
                ring.raise_to(l);
            }
        }
        LogAction::Scroll(n) => {
            // Solo aquí se cuenta lo visible: hacerlo para cada tecla recorría
            // el anillo entero también al cambiar de nivel o al abrir el
            // filtro, que no desplazan nada.
            let visibles = app
                .log_ring
                .as_ref()
                .map_or(0, |r| app.log_panel.visible_count(&r.snapshot()));
            if n < 0 {
                app.log_panel.scroll_up(n.unsigned_abs(), visibles);
            } else {
                app.log_panel
                    .scroll_down(usize::try_from(n).unwrap_or(0), visibles);
            }
        }
        LogAction::Follow => app.log_panel.follow(),
        // El filtro se teclea en el mismo campo que el resto de entradas de
        // una línea del TUI; abrirlo es lo que hace `/`.
        // Se abre con lo que ya estaba filtrando, no en blanco: afinar un
        // filtro es lo normal, y volver a teclearlo entero, no.
        LogAction::StartFilter => {
            app.log_filter_input = Some(app.log_panel.filter().to_string());
        }
        // Suelta las teclas SIN cerrar el panel: cerrar algo que el lector solo
        // quería dejar de manejar es la respuesta equivocada, y cerrarlo ya lo
        // hace `alt+l` otra vez.
        LogAction::Leave => app.return_keys_to_panes(),
    }
}

/// Resuelve por el keymap lo que este panel no reclama, y lo despacha por el
/// mismo camino que el panel de procesos (`App::processes_command`, que ya
/// atiende los `layout.*`).
fn pasar_al_keymap(
    app: &mut crate::app::App,
    resolver: &mut crate::keymap::Resolver,
    mods: crossterm::event::KeyModifiers,
    code: crossterm::event::KeyCode,
) {
    use crate::keymap::Resolution;
    let Some(chord) = crate::keymap::chord_from_crossterm(mods, code) else {
        return; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command, .. } => command,
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    app.log_command(&cmd);
}

/// Teclas mientras se escribe el filtro.
///
/// `Esc` cancela y deja el filtro ANTERIOR, no lo borra: cancelar es
/// «déjalo como estaba», y en un panel de log borrar el filtro por accidente
/// devuelve mil líneas encima de lo que estabas leyendo.
fn editar_filtro(
    app: &mut crate::app::App,
    mods: crossterm::event::KeyModifiers,
    code: crossterm::event::KeyCode,
) {
    use crossterm::event::{KeyCode, KeyModifiers};
    let Some(texto) = app.log_filter_input.as_mut() else {
        return;
    };
    match code {
        KeyCode::Char(c) if mods.is_empty() || mods == KeyModifiers::SHIFT => texto.push(c),
        KeyCode::Backspace => {
            texto.pop();
        }
        KeyCode::Enter => {
            let texto = app.log_filter_input.take().unwrap_or_default();
            app.log_panel.set_filter(texto);
        }
        KeyCode::Esc => app.log_filter_input = None,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use norte_config::logline::LogLevel;

    /// Las cinco letras de nivel están y son las iniciales del nivel en
    /// inglés, que es como se llaman en el propio log.
    #[test]
    fn cada_nivel_tiene_su_letra() {
        let esperado = [
            ('e', LogLevel::Error),
            ('w', LogLevel::Warn),
            ('i', LogLevel::Info),
            ('d', LogLevel::Debug),
            ('t', LogLevel::Trace),
        ];
        for (c, nivel) in esperado {
            assert_eq!(
                key(KeyCode::Char(c), KeyModifiers::empty()),
                Some(LogAction::Level(nivel)),
                "la tecla «{c}» no pide {nivel:?}"
            );
        }
    }

    /// Una tecla con Ctrl NO es de este panel: `ctrl+c` sale del programa y
    /// `ctrl+…` son atajos globales. Tragárselos aquí sería secuestrarlos.
    #[test]
    fn los_atajos_con_control_no_se_los_queda() {
        assert_eq!(key(KeyCode::Char('c'), KeyModifiers::CONTROL), None);
        assert_eq!(key(KeyCode::Char('d'), KeyModifiers::CONTROL), None);
    }

    /// Un acorde con modificador NO lo reclama este panel, y ahí estaba el
    /// fallo: `alt+l` es el comando que abre y cierra el registro, y mientras
    /// el panel tenía el teclado se lo tragaba entero — o sea que la misma
    /// tecla que lo abría no lo cerraba. `key` diciendo `None` es lo que manda
    /// la tecla al keymap; si algún día reclama un `alt+…`, este test cae.
    #[test]
    fn los_acordes_con_modificador_siguen_su_camino() {
        for (code, mods) in [
            (KeyCode::Char('l'), KeyModifiers::ALT),
            (KeyCode::Char('j'), KeyModifiers::ALT),
            (KeyCode::F(9), KeyModifiers::empty()),
        ] {
            assert_eq!(
                key(code, mods),
                None,
                "{code:?}+{mods:?} se lo quedó el panel en vez de dejarlo pasar"
            );
        }
    }

    /// El panel SUBE el nivel del anillo y NUNCA lo baja, y cerrar el panel es
    /// lo único que lo devuelve a donde estaba.
    ///
    /// Las tres mitades importan. Sin subirlo, filtrar a DEBUG lo que se guardó
    /// a INFO no enseña nada y parece roto. Sin el «nunca baja», ir a DEBUG,
    /// volver a WARN y pedir DEBUG otra vez borraría justo el rato que estabas
    /// investigando. Y sin bajarlo al cerrar, una sola pulsación de `t` deja el
    /// proceso capturando TRACE el resto de la sesión, con su coste, mucho
    /// después de que nadie mire.
    #[test]
    fn el_nivel_del_anillo_sube_no_baja_y_vuelve_al_cerrar() {
        use norte_config::logring::LogRing;
        let mut app = crate::app::testutil::app_dos_panes();
        let anillo = LogRing::new(10);
        app.log_ring = Some(anillo.clone());
        app.toggle_log(); // abre y toma el teclado

        aplicar_accion(&mut app, LogAction::Level(LogLevel::Debug));
        assert_eq!(anillo.level(), LogLevel::Debug, "pedir DEBUG no lo subió");
        aplicar_accion(&mut app, LogAction::Level(LogLevel::Warn));
        assert_eq!(
            anillo.level(),
            LogLevel::Debug,
            "bajar lo que se ENSEÑA no puede dejar de capturar"
        );
        assert_eq!(app.log_panel.level(), LogLevel::Warn);

        app.toggle_log(); // cierra
        assert_eq!(
            anillo.level(),
            LogLevel::Warn,
            "cerrar el panel tiene que devolver el anillo a lo que se enseñaba"
        );
    }

    /// La allowlist del registro NO es la de procesos: allí `dialog.confirm`
    /// cancela la tarea bajo el cursor, y aquí no hay nada que confirmar. Un
    /// `Enter` que cancela una copia desde un visor de log es justo el
    /// accidente que una allowlist existe para impedir.
    #[test]
    fn confirmar_es_inerte_en_el_registro_y_su_propia_tecla_lo_cierra() {
        let mut app = crate::app::testutil::app_dos_panes();
        app.toggle_log();
        assert!(app.log_slot().is_some(), "no se abrió");

        // Inerte: ni cierra el panel ni cambia de dueño del teclado.
        app.log_command("dialog.confirm");
        assert!(app.log_slot().is_some());
        assert_eq!(app.key_owner(), crate::app::KeyOwner::Log);

        // Y lo suyo sí: la misma tecla que lo abrió lo cierra desde dentro.
        app.log_command("layout.log");
        assert!(app.log_slot().is_none(), "no se cerró desde dentro");
    }

    /// `End` no es «baja mucho»: tras cambiar el filtro la lista cambia de
    /// largo, y volver al final tiene que ser una orden, no una apuesta.
    #[test]
    fn el_final_es_una_orden_propia() {
        assert_eq!(
            key(KeyCode::End, KeyModifiers::empty()),
            Some(LogAction::Follow)
        );
        assert_eq!(
            key(KeyCode::PageDown, KeyModifiers::empty()),
            Some(LogAction::Scroll(PAGINA))
        );
    }
}
