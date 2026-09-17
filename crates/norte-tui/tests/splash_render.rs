//! La pantalla de arranque, PINTADA.
//!
//! Existe porque no había nada: `src/splash.rs` prueba el modelo —que se pone,
//! que se quita, qué filas lleva— y el pintor de `ui/overlays.rs` no aparecía
//! en un solo assert. Convertir el splash en portada no puso nada en rojo, que
//! es justo la señal de que lo pintado no lo comprobaba nadie.
//!
//! Lo que se fija aquí son las dos formas y la frontera entre ellas: `brief`
//! —sin secciones— ocupa la pantalla y no dibuja el cromo de un diálogo;
//! `home` —con filas numeradas— sigue siendo una caja, porque una lista que se
//! lee y se pulsa necesita el marco que la delimita.

use norte_frontend::splash::{Daemon, SplashRow, SplashSection, SplashView};
use norte_proto::VPath;
use norte_tui::app::{App, Pane};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("wire válido")
}

fn app_con_splash(sections: Vec<SplashSection>) -> App {
    let dir = vp("mem:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    app.splash = Some(SplashView {
        art: norte_frontend::splash::ART,
        version: "0.3.0-alpha.4".to_owned(),
        revision: "abc1234".to_owned(),
        daemon: Daemon::Embedded,
        sections,
    });
    app
}

fn una_seccion() -> Vec<SplashSection> {
    vec![SplashSection {
        title_key: "splash-popular",
        rows: vec![SplashRow {
            label: "casa".to_owned(),
            detail: "12".to_owned(),
            command: "nav.goto".to_owned(),
            arg: Some("mem:///casa".to_owned()),
        }],
    }]
}

fn pantalla(app: &App, ancho: u16, alto: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(ancho, alto)).expect("backend");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    (0..alto)
        .flat_map(|y| (0..ancho).map(move |x| (x, y)))
        .map(|(x, y)| terminal.backend().buffer()[(x, y)].symbol().to_owned())
        .collect()
}

/// Sin secciones es PORTADA: el logo, y ningún cromo de diálogo.
///
/// El título del diálogo es la señal, y no los caracteres de borde: en modo
/// caja el splash se pinta ENCIMA del listado, que tiene los suyos, así que
/// buscar bordes no distinguiría una forma de la otra.
#[test]
fn sin_secciones_la_pantalla_de_arranque_es_una_portada() {
    let app = app_con_splash(Vec::new());
    let visto = pantalla(&app, 80, 24);

    assert!(visto.contains("N O R T E"), "el logo se pinta: {visto:?}");
    assert!(
        visto.contains("0.3.0-alpha.4"),
        "y debajo dice qué build corre"
    );
    assert!(
        !visto.contains(&norte_i18n::t("splash-title")),
        "una portada no lleva el título de algo que haya que cerrar"
    );
}

/// Con filas numeradas sigue siendo una CAJA, con su título y su lista.
#[test]
fn con_secciones_la_pantalla_de_arranque_sigue_siendo_una_caja() {
    let app = app_con_splash(una_seccion());
    let visto = pantalla(&app, 80, 24);

    assert!(
        visto.contains(&norte_i18n::t("splash-title")),
        "la caja se anuncia: {visto:?}"
    );
    assert!(visto.contains("casa"), "y enseña la fila");
    assert!(visto.contains('1'), "con su número, que es lo que la abre");
}

/// El logo cabe en una terminal estrecha sin partirse por la mitad.
///
/// Ochenta columnas es lo ancho; en 40 el arte mide 35 y sigue entrando. Lo
/// que este test protege es el día que alguien haga el logo más ancho sin
/// mirar: se vería cortado, y eso en una portada es lo único que hay.
#[test]
fn el_logo_entra_en_una_terminal_estrecha() {
    let app = app_con_splash(Vec::new());
    let visto = pantalla(&app, 40, 20);
    assert!(
        visto.contains("N O R T E"),
        "entra en 40 columnas: {visto:?}"
    );
}
