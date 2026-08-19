//! La caché de capacidades por `(scheme, authority)`: qué la reutiliza, qué
//! la invalida y dónde está su tope. La primera página de un `cd` es quien
//! las trae, y `apply_cd` quien las guarda.

use norte_proto::VPath;
use norte_tui::app::{App, Pane};
use norte_tui::navigate::{cache_capabilities, needs_capabilities};

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire de test")
}

fn app_en(dir: &VPath) -> App {
    App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir.clone(), Vec::new()),
    )
}

/// H3d: `fs.capabilities` devuelve catálogo Y flags en una respuesta, y
/// las dos mitades se cachean. La que se tiraba era la de los flags, y
/// tirarla costaba una ronda de red extra la próxima vez que alguien
/// preguntase si el pane era de solo lectura.
#[test]
fn se_cachean_las_dos_mitades_de_una_respuesta() {
    let dir = vp("mem:///");
    let mut app = app_en(&dir);
    let caps = norte_proto::Capabilities {
        flags: norte_proto::CapabilityFlags::READ_ONLY,
        max_path: None,
    };
    let catalog = norte_proto::AttrCatalog::new(Vec::new());

    cache_capabilities(&mut app, &dir, (caps, catalog));

    assert_eq!(app.caps(&dir), Some(&caps), "los flags se quedaron");
    assert!(
        app.attr_catalog("mem").is_some(),
        "y el catálogo, que es la mitad que ya se guardaba"
    );
    // Y el efecto que la ayuda consume: con el flag puesto, el pane es de
    // solo lectura sin volver a preguntar a nadie.
    assert!(app.pane_read_only(0));
}

/// MAJOR-1, la otra mitad: la puerta que decide si se pregunta tiene que
/// preguntar lo MISMO que responde el caché. Gateada solo por el catálogo
/// —que es por scheme—, un `cd` a un segundo host de `sftp` no volvía a
/// llamar jamás, así que las caps del primero contestaban por él durante
/// toda la sesión.
#[test]
fn otra_authority_del_mismo_scheme_vuelve_a_preguntar() {
    let a = vp("sftp://a.org/");
    let b = vp("sftp://b.org/");
    let mut app = app_en(&a);
    assert!(needs_capabilities(&app, &a), "sin nada cacheado, se pide");

    cache_capabilities(
        &mut app,
        &a,
        (
            norte_proto::Capabilities {
                flags: norte_proto::CapabilityFlags::READ_ONLY,
                max_path: None,
            },
            norte_proto::AttrCatalog::new(Vec::new()),
        ),
    );

    assert!(
        !needs_capabilities(&app, &a),
        "al mismo host no se le pregunta dos veces"
    );
    assert!(
        needs_capabilities(&app, &b),
        "b.org no ha contestado nunca: hay que preguntarle a ÉL"
    );
}

/// #215: otro DIRECTORIO del mismo backend vuelve a preguntar.
///
/// Desde ADR 0054 el daemon contesta por UBICACIÓN, y la caché seguía
/// indexando por conexión: bajo un mismo `file://` hay montajes —un pincho
/// exFAT que pliega caja, un subárbol ext4 en `+F`, un bind de solo
/// lectura— y la respuesta de `/home` se servía para todos ellos.
#[test]
fn otro_directorio_del_mismo_backend_vuelve_a_preguntar() {
    let casa = vp("file:///home/yo");
    let pincho = vp("file:///media/pincho");
    let mut app = app_en(&casa);

    cache_capabilities(
        &mut app,
        &casa,
        (
            norte_proto::Capabilities {
                flags: norte_proto::CapabilityFlags::empty(),
                max_path: None,
            },
            norte_proto::AttrCatalog::new(Vec::new()),
        ),
    );

    assert!(!needs_capabilities(&app, &casa));
    assert!(
        needs_capabilities(&app, &pincho),
        "un montaje distinto contesta por su cuenta"
    );
    assert!(
        app.caps(&pincho).is_none(),
        "y hasta que conteste, no hay respuesta suya que servir"
    );
}

/// La caché tiene TOPE: una clave por directorio ya no está acotada por
/// los siete schemes que existen, y recorrer un árbol grande la haría
/// crecer sin fin. Se desaloja el más viejo por orden de llegada.
#[test]
fn la_cache_de_capacidades_tiene_tope() {
    let primero = vp("file:///d0");
    let mut app = app_en(&primero);
    let caps = norte_proto::Capabilities {
        flags: norte_proto::CapabilityFlags::empty(),
        max_path: None,
    };
    for i in 0..200 {
        app.insert_caps(&vp(&format!("file:///d{i}")), caps);
    }
    assert!(
        app.caps(&primero).is_none(),
        "el primero se fue al llenarse"
    );
    assert!(app.caps(&vp("file:///d199")).is_some(), "y el último sigue");
}

/// La costura entera, contra un backend REAL: `first_page` con la puerta
/// abierta trae las caps y el `cd` las guarda.
///
/// Sin esto, el cableado podía revertirse en silencio y la suite quedaba
/// verde: `App::pane_read_only` cae al criterio SINTÁCTICO del scheme
/// cuando no hay caps, y hoy los dos coinciden en todo provider que
/// existe. Ninguna otra prueba distingue «llegaron los flags» de «el
/// scheme lo parecía».
#[tokio::test]
async fn la_primera_pagina_trae_las_caps_y_el_cd_las_guarda() {
    use norte_core::backend::Backend;
    use std::sync::Arc;

    let engine = norte_core::Engine::new();
    engine.register_provider(Arc::new(norte_testkit::MemProvider::new()));
    let backend = Backend::Embedded(Arc::new(engine));
    let dir = vp("mem:///");

    let (_first, _stream, _skipped, both) =
        norte_tui::navigate::first_page(&backend, &dir, &[], true)
            .await
            .expect("el listado del provider de memoria");
    let both = both.expect("con la puerta abierta llegan las DOS mitades");

    let mut app = app_en(&dir);
    assert!(app.caps(&dir).is_none());
    cache_capabilities(&mut app, &dir, both);
    assert!(
        app.caps(&dir).is_some(),
        "las caps de la respuesta tienen que quedarse en el caché"
    );

    // Y con la puerta CERRADA no se pregunta: el cuarto elemento es None.
    let (_f, _s, _k, ninguna) = norte_tui::navigate::first_page(&backend, &dir, &[], false)
        .await
        .expect("el listado igual");
    assert!(
        ninguna.is_none(),
        "con la puerta cerrada no hay ronda extra"
    );
}
