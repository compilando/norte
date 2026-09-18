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

/// Toda clave LITERAL que el código pide tiene que existir en el catálogo.
///
/// El hueco que tapa: `t("id-desconocido")` no falla, cae al propio id —eso es
/// deliberado, una cadena que falta jamás tumba la aplicación— y lo que se
/// pinta es `msg-organize-hidden` en la barra de estado. La paridad entre
/// locales de arriba tampoco lo ve: una clave que no está en NINGUNO de los
/// dos está igual de ausente en los dos. Así que la única forma de enterarse
/// era mirar la pantalla, y una barra que solo aparece en un caso raro no la
/// mira nadie. Pasó con `msg-organize-hidden` en la fase 8.
///
/// Solo claves literales, y a propósito: las que se COMPONEN (`help-cmd-` +
/// el nombre del comando, los ids del catálogo RPC) tienen sus propias
/// puertas —el golden del CLI las pinta todas— y perseguirlas aquí pediría
/// evaluar el código en vez de leerlo.
#[test]
fn toda_clave_literal_del_codigo_existe_en_el_catalogo() {
    let ids = message_ids(Lang::Es);
    let raiz = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/");
    let mut faltan: Vec<(String, String)> = Vec::new();
    let mut vistas = 0usize;
    for crate_dir in std::fs::read_dir(raiz).expect("crates/") {
        let src = crate_dir.expect("entrada").path().join("src");
        if !src.is_dir() {
            continue;
        }
        for fichero in ficheros_rust(&src) {
            let texto = std::fs::read_to_string(&fichero).expect("fuente utf-8");
            for clave in claves_de(&texto) {
                vistas += 1;
                if !ids.contains(&clave) {
                    faltan.push((fichero.display().to_string(), clave));
                }
            }
        }
    }
    assert!(vistas > 100, "el barrido no encontró claves: {vistas}");
    assert!(
        faltan.is_empty(),
        "claves que el código pide y el catálogo no tiene: {faltan:?}"
    );
}

/// Todos los `.rs` bajo `dir`, recursivamente.
fn ficheros_rust(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut pendientes = vec![dir.to_path_buf()];
    while let Some(d) = pendientes.pop() {
        let Ok(entradas) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entradas.flatten() {
            let p = e.path();
            if p.is_dir() {
                pendientes.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out
}

/// Las claves literales de `t("…")` / `ta("…", …)` y sus variantes `_in`.
///
/// Un `t(clave)` con variable no casa —no hay literal que leer— y eso es lo
/// que hace que el barrido no dé falsos positivos.
fn claves_de(texto: &str) -> Vec<String> {
    let mut out = Vec::new();
    for llamada in ["t(\"", "ta(\"", "t_in(self.lang, \"", "ta_in(self.lang, \""] {
        let mut resto = texto;
        while let Some(i) = resto.find(llamada) {
            // Que la llamada no sea el final de otro identificador
            // (`format(`, `debug_assert(`…): delante tiene que haber algo que
            // no sea parte de un nombre.
            let antes = resto[..i].chars().next_back();
            let limpia = antes.is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != ':');
            let tras = &resto[i + llamada.len()..];
            if limpia && let Some(fin) = tras.find('"') {
                let clave = &tras[..fin];
                // Una clave de verdad es kebab-case: así una cadena
                // cualquiera que empiece por `t("` no entra.
                if !clave.is_empty()
                    && clave
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                {
                    out.push(clave.to_owned());
                }
            }
            resto = tras;
        }
    }
    out
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
