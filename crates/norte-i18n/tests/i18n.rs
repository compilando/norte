//! Tests del i18n (issue #1): paridad TOTAL de ids entre locales, fallback
//! visible (jamás panic) y negociación de idioma.

use norte_i18n::{Lang, message_ids, t_in, ta_in};

#[test]
fn los_dos_locales_tienen_exactamente_los_mismos_ids() {
    let es = message_ids(Lang::Es);
    let en = message_ids(Lang::En);
    let falta_en_ingles: Vec<_> = es.iter().filter(|i| !en.contains(i)).collect();
    let falta_en_espanol: Vec<_> = en.iter().filter(|i| !es.contains(i)).collect();
    assert!(
        falta_en_ingles.is_empty() && falta_en_espanol.is_empty(),
        "ids sin traducir — faltan en en.ftl: {falta_en_ingles:?}; faltan en es.ftl: {falta_en_espanol:?}"
    );
    assert!(!es.is_empty(), "el catálogo no puede estar vacío");
}

#[test]
fn traduce_en_ambos_idiomas() {
    assert_eq!(t_in(Lang::Es, "modal-trash-title"), "A la papelera");
    assert_eq!(t_in(Lang::En, "modal-trash-title"), "To trash");
}

#[test]
fn id_desconocido_cae_al_propio_id_sin_panic() {
    // Un id con typo se VE en la UI (greppeable), jamás revienta.
    assert_eq!(t_in(Lang::Es, "id-inventado-xyz"), "id-inventado-xyz");
}

#[test]
fn args_de_fluent() {
    let msg = ta_in(Lang::Es, "msg-error", &[("error", "not found")]);
    assert!(msg.contains("not found"), "{msg}");
}

#[test]
fn negociacion_de_idioma() {
    assert_eq!(Lang::negotiate(Some("es_ES.UTF-8")), Lang::Es);
    assert_eq!(Lang::negotiate(Some("es")), Lang::Es);
    assert_eq!(Lang::negotiate(Some("en_US.UTF-8")), Lang::En);
    assert_eq!(Lang::negotiate(Some("de_DE")), Lang::En, "fallback en");
    assert_eq!(Lang::negotiate(None), Lang::En);
}
