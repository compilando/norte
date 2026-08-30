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

/// **Guardar como perfil se abre PRELLENADO con el perfil activo** (#306).
///
/// Lo normal es partir del que tienes puesto, así que «guardar como» sobre el
/// mismo nombre es guardar encima — que es lo que hace cualquier programa. Sin
/// perfil el campo nace vacío: no hay un nombre por defecto que no sea una
/// invención.
#[test]
fn guardar_como_perfil_parte_del_activo() {
    let mut app = app_de_prueba();
    app.open_profile_save_as();
    assert!(
        matches!(&app.modal, Some(norte_tui::app::Modal::ProfileSaveAs { name, .. }) if name.is_empty()),
        "sin perfil, vacío: {:?}",
        app.modal
    );

    app.modal = None;
    app.active_profile = Some(OsString::from("fotos"));
    app.open_profile_save_as();
    assert!(
        matches!(&app.modal, Some(norte_tui::app::Modal::ProfileSaveAs { name, .. }) if name == "fotos"),
        "con perfil, el suyo: {:?}",
        app.modal
    );
}

/// Y un nombre que no puede ser un directorio deja el modal abierto con su
/// diagnóstico: lo tecleado sobrevive para corregirlo, que es la disciplina de
/// los prompts de esta pantalla.
#[tokio::test]
async fn un_nombre_de_perfil_invalido_no_cierra_el_modal() {
    let mut app = app_de_prueba();
    app.open_profile_save_as();
    let Some(norte_tui::app::Modal::ProfileSaveAs { name, .. }) = &mut app.modal else {
        panic!("el modal está abierto");
    };
    name.push_str("../otro");

    norte_tui::screens::profile_save_as(&mut app).await;

    assert!(
        matches!(
            &app.modal,
            Some(norte_tui::app::Modal::ProfileSaveAs { name, error: Some(_) }) if name == "../otro"
        ),
        "sigue abierto, con el nombre y el motivo: {:?}",
        app.modal
    );
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
