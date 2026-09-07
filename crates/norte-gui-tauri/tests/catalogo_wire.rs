//! La forma del `HostCatalog` en el cable, clavada.
//!
//! Es el quinto mensaje que cruza a la webview y el único que no vive en
//! `norte-ui-host`, así que el corpus golden del puente no lo cubría: un
//! campo renombrado aquí dejaba al renderer leyendo `undefined` sin que nada
//! se pusiera rojo (#259). Sigue viviendo en este crate —lo que lleva son
//! textos ya traducidos y colores, cosa de quien pinta— pero con red.

use norte_gui_tauri::catalog::HostCatalog;

/// La fixture: los campos exactos que `ui/src/types.ts` declara.
const FIXTURE: &str = r##"{
  "bridge_version": 36,
  "instance_id": "host-1",
  "locale": "es",
  "strings": { "hostile-name": "nombre alterado" },
  "theme": { "bg": "#101216" },
  "measure": false,
  "busy_threshold_ms": 250,
  "appearance": {
    "font": "Inter",
    "mono_font": "Iosevka",
    "font_size": 15.0,
    "reduce_motion": true
  }
}"##;

/// Ida y vuelta: los nombres del JSON son el contrato, no los de Rust.
#[test]
fn el_catalogo_va_y_vuelve_con_los_mismos_campos() {
    let leido: HostCatalog = serde_json::from_str(FIXTURE).expect("la fixture deserializa");
    assert_eq!(leido.instance_id, "host-1");
    assert_eq!(leido.locale, "es");
    assert_eq!(leido.strings["hostile-name"], "nombre alterado");
    assert_eq!(leido.theme["bg"], "#101216");
    assert!(!leido.measure);
    // El umbral de la espera viaja: escribirlo en el CSS sería un tercer
    // sitio donde vive el mismo número, y el primero donde olvidarse.
    assert_eq!(
        u128::from(leido.busy_threshold_ms),
        norte_frontend::busy::THRESHOLD.as_millis(),
        "el catálogo lleva el umbral COMPARTIDO, no una copia"
    );

    // Las cuatro claves de `[ui]` que se cargaban, se validaban, se ofrecían
    // en la pantalla de ajustes y no leía nadie. `reduce_motion` además es un
    // compromiso de accesibilidad de la spec §17.
    assert_eq!(leido.appearance.font.as_deref(), Some("Inter"));
    assert_eq!(leido.appearance.mono_font.as_deref(), Some("Iosevka"));
    assert_eq!(leido.appearance.font_size, Some(15.0));
    assert_eq!(leido.appearance.reduce_motion, Some(true));

    let vuelta: serde_json::Value = serde_json::to_value(&leido).expect("serializa");
    let esperado: serde_json::Value = serde_json::from_str(FIXTURE).expect("json");
    assert_eq!(
        vuelta, esperado,
        "el catálogo tiene que volver con los MISMOS campos: un renombrado \
         aquí deja al renderer leyendo `undefined`"
    );
}

/// El número que el catálogo lleva es el del host, no una copia a mano.
///
/// Es informativo —la compatibilidad la decide el renderer sobre el SOBRE que
/// va a interpretar—, pero un número que se queda atrás en el único mensaje
/// que un humano mira al diagnosticar es peor que no llevarlo.
#[test]
fn el_catalogo_lleva_la_version_del_host() {
    let tema = norte_theme::Theme::default();
    let instancia = norte_ui_host::InstanceId::new("host-1".to_owned());
    let cat = norte_gui_tauri::catalog::catalogo(&instancia, norte_i18n::Lang::Es, &tema);
    assert_eq!(cat.bridge_version, norte_ui_host::BRIDGE_VERSION);
}
