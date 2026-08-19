//! Abrir la ayuda congela los hechos del contexto: el overlay pinta lo que
//! era cierto al abrirlo, no lo que sea cierto mientras está abierto.

use norte_help::ChordResolver as _;
use norte_proto::VPath;
use norte_tui::app::{App, Pane};
use norte_tui::overlays::open_contextual_help;

/// Abrir la ayuda CONGELA los hechos del contexto (H3d).
///
/// El resto de la cadena —la tabla compartida, el resolver, el pintor de la
/// razón— tiene sus propios tests y seguiría VERDE con esta llamada
/// borrada: el overlay se pintaría contra el resolver permisivo del
/// arranque y ninguna fila se atenuaría jamás. Este test es el único que
/// mira el eslabón.
///
/// Se abre desde dentro de un zip (`READ_ONLY` por el scheme, ADR 0018) con
/// los dos panes ahí: sin destino escribible, `pane.copy` no puede correr.
#[test]
fn abrir_la_ayuda_congela_los_hechos_del_contexto() {
    let inside = VPath::parse("zip+file:///a.zip/!").expect("wire de test");
    let mut app = App::new(
        Pane::new(inside.clone(), Vec::new()),
        Pane::new(inside, Vec::new()),
    );
    assert!(
        app.help_chords.availability("pane.copy").is_available(),
        "antes de abrir, el resolver del arranque no atenúa nada"
    );

    open_contextual_help(&mut app, norte_help::Lang::En, &[], None);

    assert!(app.help.is_some(), "el overlay se abrió");
    assert_eq!(
        app.help_chords.availability("pane.copy").reason(),
        Some(norte_help::Reason::ReadOnlyBackend),
        "la ayuda tiene que saber que está dentro de un archivo"
    );
}
