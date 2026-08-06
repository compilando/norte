//! El overlay del gestor de extensiones (M4-P3): navegación, toggle local de
//! feedback y render. La lógica vive en `ExtensionManager` (dentro de `App`),
//! así que se testea sin el bucle de eventos. El render se comprueba con un
//! `TestBackend`, incluido el enmascarado de un `name` hostil (el name es texto
//! libre de un tercero — superficie de decisión de seguridad, spec §6).

use norte_proto::methods::{PluginInfo, PluginLoadError};
use norte_tui::app::{App, ExtensionManager, Pane};
use norte_tui::ui;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn plugin(id: &str, name: &str, category: &str, caps: &[&str], approved: bool) -> PluginInfo {
    PluginInfo {
        id: id.into(),
        name: name.into(),
        publisher: "acme".into(),
        version: "1.0.0".into(),
        category: category.into(),
        capabilities: caps.iter().map(|c| (*c).to_string()).collect(),
        approved,
        enabled: true,
        description: None,
        commands: Vec::new(),
        columns: Vec::new(),
        has_help: false,
    }
}

fn mgr() -> ExtensionManager {
    ExtensionManager {
        // YA ordenados por categoría e id (como los entrega el core).
        plugins: vec![
            plugin("org.a.idx", "Alpha Indexer", "indexer", &["fs-read"], true),
            plugin("org.b.prev", "Beta Preview", "previewer", &[], false),
            plugin(
                "org.c.prev",
                "Gamma Preview",
                "previewer",
                &["fs-read"],
                true,
            ),
        ],
        errors: Vec::new(),
        cursor: 0,
        config: None,
    }
}

fn app_with(mgr: ExtensionManager) -> App {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let d = norte_proto::VPath::parse("file:///x").expect("wire");
    let mut app = App::new(Pane::new(d.clone(), Vec::new()), Pane::new(d, Vec::new()));
    app.extensions = Some(mgr);
    app
}

#[test]
fn navegacion_clampa_y_selecciona() {
    let mut m = mgr();
    assert_eq!(m.selected().map(|p| p.id.as_str()), Some("org.a.idx"));
    m.up(); // tope arriba
    assert_eq!(m.cursor, 0);
    m.down();
    m.down();
    assert_eq!(m.selected().map(|p| p.id.as_str()), Some("org.c.prev"));
    m.down(); // tope abajo (3 plugins)
    assert_eq!(m.cursor, 2);
}

#[test]
fn toggle_local_muta_el_bool_bajo_el_cursor() {
    let mut m = mgr();
    m.down(); // Beta Preview, sin aprobar
    assert!(!m.selected().unwrap().approved);
    m.set_local_approved(true);
    assert!(m.selected().unwrap().approved);
    // No tocó a los demás.
    assert!(m.plugins[0].approved);
    m.set_local_enabled(false);
    assert!(!m.selected().unwrap().enabled);
}

#[test]
fn selected_vacio_es_none() {
    let m = ExtensionManager {
        plugins: Vec::new(),
        errors: Vec::new(),
        cursor: 0,
        config: None,
    };
    assert!(m.selected().is_none());
}

#[test]
fn render_muestra_nombre_badge_y_aviso() {
    let app = app_with(mgr());
    let mut t = Terminal::new(TestBackend::new(80, 24)).expect("term");
    t.draw(|f| ui::draw(f, &app)).expect("draw");
    let texto = t.backend().to_string();
    // Un nombre de plugin.
    assert!(texto.contains("Alpha Indexer"), "falta el nombre: {texto}");
    // Un capability badge.
    assert!(
        texto.contains("fs-read"),
        "falta el badge de capability: {texto}"
    );
    // El aviso de no-aprobado (Beta Preview).
    assert!(texto.contains("sin aprobar"), "falta el aviso: {texto}");
    // Cabecera de grupo por categoría.
    assert!(
        texto.contains("previewer"),
        "falta la cabecera de grupo: {texto}"
    );
}

#[test]
fn render_overlay_vacio_muestra_ext_empty() {
    let app = app_with(ExtensionManager {
        plugins: Vec::new(),
        errors: Vec::new(),
        cursor: 0,
        config: None,
    });
    let mut t = Terminal::new(TestBackend::new(60, 12)).expect("term");
    t.draw(|f| ui::draw(f, &app)).expect("draw");
    assert!(t.backend().to_string().contains("no hay extensiones"));
}

#[test]
fn render_enmascara_nombre_hostil() {
    // Un name con un control char (BEL): superficie de decisión de seguridad
    // (aprobar). El render NO debe pintar el byte crudo — lo enmascara a �.
    let app = app_with(ExtensionManager {
        plugins: vec![plugin(
            "org.evil.x",
            "mal\u{0007}o",
            "previewer",
            &[],
            false,
        )],
        errors: Vec::new(),
        cursor: 0,
        config: None,
    });
    let mut t = Terminal::new(TestBackend::new(60, 12)).expect("term");
    t.draw(|f| ui::draw(f, &app)).expect("draw");
    let texto = t.backend().to_string();
    assert!(
        !texto.contains('\u{0007}'),
        "el byte de control se pintó crudo: {texto:?}"
    );
    assert!(
        texto.contains('\u{FFFD}'),
        "no se marcó el enmascarado: {texto:?}"
    );
}

/// P1 encoding audit F1 (MEDIUM): un daemon hostil/comprometido puede mandar
/// una `description` de CUALQUIER longitud por el wire (el manifiesto solo
/// limita a 280 chars al PARSEAR, un chequeo del camino honesto que un
/// daemon fiel respeta pero uno hostil no tiene por qué). Este test
/// construye `ExtensionManager` DIRECTO (como haría cualquier caller que no
/// pase por `main::dispatch`'s ingest, `clamp_plugin_descriptions`), con una
/// description sin tope, y solo comprueba que el render no panica ni se
/// cuelga — el efecto de acotar en `ui::plugin_description_line` no es
/// visible en el frame renderizado (el popup ya acota lo VISIBLE por ancho,
/// con o sin el tope de 280: `plugin_description_line`'s propio unit test
/// en `ui.rs` prueba el tope directo, sin pasar por el layout).
#[test]
fn render_no_panica_con_description_sin_tope_del_wire() {
    let mut p = plugin("org.norte.demo", "Demo", "previewer", &[], true);
    p.description = Some("a".repeat(50_000));
    let app = app_with(ExtensionManager {
        plugins: vec![p],
        errors: Vec::new(),
        cursor: 0,
        config: None,
    });
    let mut t = Terminal::new(TestBackend::new(60, 12)).expect("term");
    t.draw(|f| ui::draw(f, &app)).expect("draw");
}

#[test]
fn render_muestra_errores_de_carga() {
    let app = app_with(ExtensionManager {
        plugins: Vec::new(),
        errors: vec![PluginLoadError {
            dir: "/plugins/roto".into(),
            reason: "manifiesto inválido".into(),
        }],
        cursor: 0,
        config: None,
    });
    let mut t = Terminal::new(TestBackend::new(70, 12)).expect("term");
    t.draw(|f| ui::draw(f, &app)).expect("draw");
    let texto = t.backend().to_string();
    assert!(texto.contains("roto"), "falta el dir del error: {texto}");
    assert!(texto.contains("inválido"), "falta el motivo: {texto}");
}
