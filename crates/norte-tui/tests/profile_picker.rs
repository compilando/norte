//! El selector de perfiles visto desde el `App`: qué pide al confirmar y qué
//! no, y que su caja se pinta sin desbordar.

use std::ffi::OsString;

use norte_frontend::profile_picker::UserProfile;
use norte_proto::VPath;
use norte_tui::app::{App, Pane, PickerAction};

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("wire válido")
}

fn app_de_prueba() -> App {
    App::new(
        Pane::new(vp("file:///izq"), Vec::new()),
        Pane::new(vp("file:///der"), Vec::new()),
    )
}

fn perfil(name: &str) -> UserProfile {
    UserProfile {
        name: OsString::from(name),
        title: None,
        problem: None,
    }
}

/// Confirmar deja el cambio PEDIDO y cierra el selector. No lo hace aquí: el
/// cambio recarga configuración, y hacerlo desde el manejador de una tecla es
/// la regla 2 otra vez.
#[test]
fn confirmar_pide_el_cambio_y_cierra() {
    let mut app = app_de_prueba();
    app.open_profile_picker(vec![perfil("work"), perfil("photos")]);
    app.profile_picker_input(PickerAction::Down);
    app.profile_picker_input(PickerAction::Confirm);

    assert!(app.profile_picker.is_none(), "el selector se cierra");
    assert_eq!(
        app.pending_profile.as_deref(),
        Some(std::ffi::OsStr::new("photos"))
    );
}

/// Elegir el perfil que YA está activo no pide nada: un cambio que no cambia
/// nada tiraría y recargaría la pantalla para dejarla igual.
#[test]
fn confirmar_el_activo_no_pide_nada() {
    let mut app = app_de_prueba();
    app.active_profile = Some(OsString::from("work"));
    app.open_profile_picker(vec![perfil("work")]);
    app.profile_picker_input(PickerAction::Confirm);

    assert!(app.profile_picker.is_none());
    assert_eq!(app.pending_profile, None);
}

/// Cancelar cierra y no pide nada.
#[test]
fn cancelar_no_pide_nada() {
    let mut app = app_de_prueba();
    app.open_profile_picker(vec![perfil("work")]);
    app.profile_picker_input(PickerAction::Cancel);
    assert!(app.profile_picker.is_none());
    assert_eq!(app.pending_profile, None);
}

/// La caja se pinta sin desbordar en un terminal pequeño, con la lista vacía
/// y con filas que piden nota. Una lista vacía no es un error: es que todavía
/// no has creado ninguno.
#[test]
fn se_pinta_sin_desbordar() {
    for perfiles in [
        Vec::new(),
        vec![perfil("work")],
        vec![
            perfil("orthodox"),
            UserProfile {
                name: OsString::from("roto"),
                title: Some("Un título largo de verdad".to_owned()),
                problem: Some("línea 3: unknown field `them`".to_owned()),
            },
        ],
    ] {
        for (w, h) in [(24_u16, 6_u16), (80, 24), (200, 60)] {
            let mut app = app_de_prueba();
            app.open_profile_picker(perfiles.clone());
            let backend = ratatui::backend::TestBackend::new(w, h);
            let mut term = ratatui::Terminal::new(backend).expect("terminal");
            term.draw(|f| norte_tui::ui::draw(f, &app)).expect("pinta");
        }
    }
}
