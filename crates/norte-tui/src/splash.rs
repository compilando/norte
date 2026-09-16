//! La pantalla de arranque en la TUI (spec 2026-09-15, fase 2): cuándo se
//! pone, con qué filas, y qué hace una tecla mientras está puesta.
//!
//! El modelo es el compartido (`norte_frontend::splash`); aquí va lo que solo
//! el terminal sabe: de dónde salen las fuentes de este proceso y que el
//! plazo del `brief` se mide con el reloj del pintado.

use norte_frontend::splash::{Daemon, SplashRow, SplashSection, SplashSource, SplashView};

use crate::app::App;
use crate::config;

/// ¿Hay que poner el splash?
///
/// `off`, `--no-splash` y `NORTE_NO_SPLASH` lo apagan; `--pick` también,
/// porque ahí la salida es para otro programa. El ASISTENTE gana: si los dos
/// quisieran salir, sale el que pregunta algo, y el splash de ese arranque
/// sobra.
#[must_use]
pub fn should_open(
    modo: norte_config::load::SplashMode,
    no_splash: bool,
    pick: bool,
    wizard: bool,
) -> bool {
    use norte_config::load::SplashMode;
    if pick || wizard || no_splash || std::env::var_os("NORTE_NO_SPLASH").is_some() {
        return false;
    }
    modo != SplashMode::Off
}

/// Las secciones de ESTE proceso: a dónde sueles ir, y lo que has guardado.
///
/// `home` las enseña; `brief` no, porque una portada que se quita sola no es
/// sitio para elegir nada.
struct Populares<'a>(&'a norte_frontend::history::Popular);

impl SplashSource for Populares<'_> {
    fn section(&self) -> Option<SplashSection> {
        let rows: Vec<SplashRow> = self
            .0
            .ranked()
            .into_iter()
            .take(5)
            .map(|e| {
                let (texto, _) = norte_frontend::display::path_display(&e.path);
                SplashRow {
                    label: texto,
                    detail: e.visits.to_string(),
                    command: "nav.enter".to_owned(),
                    arg: Some(e.path.to_wire()),
                }
            })
            .collect();
        (!rows.is_empty()).then_some(SplashSection {
            title_key: "splash-popular",
            rows,
        })
    }
}

struct Favoritos<'a>(&'a [norte_config::HotlistItem]);

impl SplashSource for Favoritos<'_> {
    fn section(&self) -> Option<SplashSection> {
        let rows: Vec<SplashRow> = self
            .0
            .iter()
            .take(5)
            .filter_map(|h| {
                let destino = h.target.as_ref().ok()?;
                // El nombre lo escribió una persona en un fichero: se enmascara
                // como cualquier otro texto de tercero.
                let (nombre, _) = norte_frontend::display_name(h.name.as_bytes());
                let (ruta, _) = norte_frontend::display::path_display(destino);
                Some(SplashRow {
                    label: nombre,
                    detail: ruta,
                    command: "nav.enter".to_owned(),
                    arg: Some(destino.to_wire()),
                })
            })
            .collect();
        (!rows.is_empty()).then_some(SplashSection {
            title_key: "splash-bookmarks",
            rows,
        })
    }
}

/// Pone el splash, con las filas que correspondan al modo.
///
/// `brief` va SIN secciones a propósito: se quita sola, así que una lista de
/// sitios ahí sería una oferta que se retira antes de poder aceptarla.
pub fn open(app: &mut App, modo: norte_config::load::SplashMode, cfg: &config::LoadedConfig) {
    use norte_config::load::SplashMode;
    let sections = if modo == SplashMode::Home {
        let populares = Populares(&app.popular);
        let favoritos = Favoritos(&cfg.common.hotlist);
        let fuentes: [&dyn SplashSource; 2] = [&populares, &favoritos];
        norte_frontend::splash::sections(&fuentes)
    } else {
        Vec::new()
    };
    // La línea que ya imprime `--version`: versión Y revisión de git, escritas
    // una sola vez. Dos formas de decir qué build corre acaban diciendo cosas
    // distintas el día que una se queda atrás.
    let version = norte_frontend::version::VERSION.to_owned();
    let revision = norte_frontend::version::VERSION_LINE
        .split_once(' ')
        .map_or_else(String::new, |(_, resto)| resto.to_owned());
    app.splash = Some(SplashView {
        art: norte_frontend::splash::ART,
        version,
        revision,
        // Lo que este proceso tiene delante: el embebido o un daemon. Se
        // decide fuera y llega hecho para no preguntarle a la config lo que
        // sabe el backend.
        daemon: if app.backend_journalled {
            Daemon::Connected
        } else {
            Daemon::Embedded
        },
        sections,
    });
    // El plazo sale de `[ui] splash_ms`, no de una constante: una portada que
    // no da tiempo a leerse solo estorba, y cuánto es «tiempo» depende de
    // quién mira.
    app.splash_until_ms = (modo == SplashMode::Brief)
        .then(|| app.now_ms() + i64::from(cfg.common.ui_chrome.splash_ms()));
}

/// Una tecla con el splash puesto: lo quita, y con `home` un número ejecuta su
/// fila.
///
/// Devuelve el `(comando, argumento)` que hay que despachar, si el número
/// nombraba una fila. Cualquier otra tecla solo quita la capa: no se traga
/// nada más, porque una portada que se come la primera tecla útil se lee como
/// que norte no responde.
pub fn on_key(app: &mut App, code: crossterm::event::KeyCode) -> Option<(String, Option<String>)> {
    use crossterm::event::KeyCode;
    let elegido = match (code, app.splash.as_ref()) {
        (KeyCode::Char(c @ '1'..='9'), Some(vista)) => {
            let n = c.to_digit(10).unwrap_or(0) as usize;
            norte_frontend::splash::numbered(&vista.sections)
                .into_iter()
                .find(|(i, _)| usize::from(*i) == n)
                .map(|(_, fila)| (fila.command.clone(), fila.arg.clone()))
        }
        _ => None,
    };
    app.splash = None;
    app.splash_until_ms = None;
    elegido
}

/// El plazo del `brief` se acabó (o nunca hubo splash): lo quita.
///
/// Lo llama el bucle después de pintar, con el mismo reloj que pinta: un
/// plazo medido con otro reloj es un plazo que los tests no pueden fijar.
pub fn tick(app: &mut App) {
    let Some(hasta) = app.splash_until_ms else {
        return;
    };
    if app.now_ms() >= hasta {
        app.splash = None;
        app.splash_until_ms = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_config::load::SplashMode;

    /// La puerta: `off` no pone nada, el asistente gana, y `--no-splash` y
    /// `--pick` lo apagan aunque la config diga otra cosa.
    #[test]
    fn la_puerta_del_splash_cede_ante_el_asistente_y_ante_las_banderas() {
        assert!(should_open(SplashMode::Brief, false, false, false));
        assert!(should_open(SplashMode::Home, false, false, false));
        assert!(!should_open(SplashMode::Off, false, false, false));
        assert!(
            !should_open(SplashMode::Home, false, false, true),
            "el asistente pregunta algo: el splash de ese arranque sobra"
        );
        assert!(
            !should_open(SplashMode::Brief, true, false, false),
            "--no-splash"
        );
        assert!(
            !should_open(SplashMode::Brief, false, true, false),
            "--pick: la salida es para otro programa"
        );
    }
}
