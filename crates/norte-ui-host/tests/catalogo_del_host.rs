//! Toda clave Fluent que el HOST elige EXISTE en los dos catálogos.
//!
//! El gemelo en Rust de `norte-gui-tauri/tests/catalogo_completo.rs`, y hacía
//! falta porque ese solo ve lo que pide el TypeScript. Estas claves las elige
//! el host —`ActionAck::Unavailable { reason_key }`, el título de un diálogo,
//! la etiqueta de cada respuesta— y viajan por el puente como datos, así que
//! el renderer las pinta con `t(key)` sin que aparezcan en ningún literal
//! suyo.
//!
//! Faltaban VEINTIUNA, y entre ellas las dos respuestas del diálogo donde un
//! humano aprueba la mutación que pidió un agente: los botones se pintaban
//! `dialog-approve` y `dialog-deny`. `t` contesta una clave ausente con la
//! clave misma, así que nada se cae — se lee.
//!
//! El test lee el CÓDIGO y no una lista escrita a mano: una lista se separa
//! del código en la primera superficie nueva, que es exactamente lo que pasó.

use std::collections::{BTreeMap, BTreeSet};

/// Los ficheros del host donde se eligen claves.
const FUENTES: &[(&str, &str)] = &[
    ("controller.rs", include_str!("../src/controller.rs")),
    ("help.rs", include_str!("../src/help.rs")),
    ("extensions.rs", include_str!("../src/extensions.rs")),
    ("settings.rs", include_str!("../src/settings.rs")),
    ("pickers.rs", include_str!("../src/pickers.rs")),
];

/// Los campos cuyo valor ES una clave Fluent.
const CAMPOS: &[&str] = &["reason_key", "title_key", "label_key"];

/// Las LLAMADAS que traducen una clave en el sitio.
///
/// Los campos de arriba solo ven las claves que VIAJAN al renderer. Una que el
/// host traduce él mismo —para la barra de estado, para una línea de un
/// diálogo— no pasa por ningún campo `*_key`, así que este barrido no la veía:
/// `err-bad-name` llevaba desde la fase 2 sin existir en ningún idioma, y
/// confirmar un nombre ilegal ponía el identificador crudo en la barra.
const LLAMADAS: &[&str] = &[
    "norte_i18n::t(",
    "norte_i18n::t_in(",
    "norte_i18n::ta(",
    "norte_i18n::ta_in(",
];

/// Las funciones COMPARTIDAS que devuelven una clave, con su fichero.
///
/// El host no las escribe, las llama, así que el barrido de literales no las
/// ve. Las tres son `match` cerrados sobre `&'static str`, o sea que su
/// vocabulario ENTERO está en su cuerpo y se puede comprobar igual.
const INDIRECTAS: &[(&str, &str, &str)] = &[
    (
        "error.rs",
        include_str!("../../norte-frontend/src/error.rs"),
        "pub fn error_key(",
    ),
    (
        "availability.rs",
        include_str!("../../norte-frontend/src/availability.rs"),
        "pub fn reason_key(",
    ),
    (
        "nav.rs",
        include_str!("../../norte-frontend/src/nav.rs"),
        "pub fn empty_message(",
    ),
];

/// Lo que se acepta como clave NO literal en un sitio del host.
///
/// Cada una está cubierta por `INDIRECTAS` o por otro sitio del propio
/// barrido, y aquí se nombra para que añadir una cuarta forma de calcular una
/// clave falle en vez de colarse.
const CALCULADAS: &[&str] = &[
    "error::error_key",
    "availability::reason_key",
    "empty_message()",
    // La devuelve `elegir_pagina`, y es una de las de `availability`.
    "reason_key: clave",
    // El motivo por el que una respuesta de diálogo NO hizo nada. Sale de
    // `bytes_del_rename` / `segmento_tecleado`, que devuelven claves
    // literales y por tanto SÍ las ve este barrido en su origen.
    "reason_key: reason_key",
];

/// Cuánto texto se mira tras un campo para encontrar sus literales.
///
/// Un `match` de tres brazos o un `if/else` caben de sobra; el corte está
/// para que un campo sin literal se detecte en vez de tragarse el resto del
/// fichero.
const VENTANA: usize = 600;

/// Un literal que puede ser una clave Fluent: minúsculas, dígitos y guiones,
/// con al menos un guion. Descarta rutas, formatos y nombres de kind.
fn parece_clave(s: &str) -> bool {
    s.contains('-')
        && !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// Las claves que el host elige, por fichero y sitio.
fn claves_elegidas() -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (nombre, fuente) in FUENTES {
        for campo in CAMPOS {
            let aguja = format!("{campo}:");
            let mut desde = 0usize;
            while let Some(i) = fuente[desde..].find(&aguja) {
                let inicio = desde + i + aguja.len();
                desde = inicio;
                let fin = (inicio + VENTANA).min(fuente.len());
                let ventana = &fuente[inicio..fin];
                let encontradas: Vec<String> = literales(ventana)
                    .into_iter()
                    .filter(|s| parece_clave(s))
                    .collect();
                let linea = fuente[..inicio].lines().count();
                if encontradas.is_empty() {
                    // Un campo cuyo valor se calcula solo vale si lo calcula
                    // algo que este test SÍ mira.
                    // Con el nombre del campo delante: es parte de la forma
                    // que se reconoce.
                    let desde_campo = inicio.saturating_sub(aguja.len() + 2);
                    let cabecera = &fuente[desde_campo..(inicio + 80).min(fuente.len())];
                    assert!(
                        CALCULADAS.iter().any(|c| cabecera.contains(c)),
                        "{nombre}:{linea}: `{campo}` sin literal y sin estar en \
                         `CALCULADAS`. Una clave que este test no puede seguir es \
                         una clave que se pintará como su propio identificador el \
                         día que falte: o es literal, o su origen se nombra aquí."
                    );
                    continue;
                }
                out.entry(format!("{nombre}:{linea}"))
                    .or_default()
                    .extend(encontradas);
            }
        }
    }
    // Las que el host traduce en el sitio. Se toma el PRIMER literal que
    // parezca una clave dentro de la ventana: `t_in` lleva el idioma delante y
    // `ta_in` los argumentos detrás, así que el primero es siempre el id.
    for (nombre, fuente) in FUENTES {
        for llamada in LLAMADAS {
            let mut desde = 0usize;
            while let Some(i) = fuente[desde..].find(llamada) {
                let inicio = desde + i + llamada.len();
                desde = inicio;
                let fin = (inicio + VENTANA).min(fuente.len());
                let encontradas: Vec<String> = literales(&fuente[inicio..fin])
                    .into_iter()
                    .filter(|s| parece_clave(s))
                    .take(1)
                    .collect();
                if encontradas.is_empty() {
                    // Una clave calculada: la trae una variable, y su origen
                    // tiene que ser algo que este test SÍ mire.
                    continue;
                }
                let linea = fuente[..inicio].lines().count();
                out.entry(format!("{nombre}:{linea}"))
                    .or_default()
                    .extend(encontradas);
            }
        }
    }
    for (nombre, fuente, firma) in INDIRECTAS {
        let i = fuente
            .find(firma)
            .unwrap_or_else(|| panic!("{nombre}: `{firma}` ya no está ahí"));
        let cuerpo = &fuente[i..];
        let fin = cuerpo.find("\n}").unwrap_or(cuerpo.len());
        let claves: Vec<String> = literales(&cuerpo[..fin])
            .into_iter()
            .filter(|s| parece_clave(s))
            .collect();
        assert!(
            !claves.is_empty(),
            "{nombre}: `{firma}` no devuelve ninguna clave literal; el barrido \
             dejó de ver su vocabulario"
        );
        let linea = fuente[..i].lines().count();
        out.entry(format!("{nombre}:{linea}"))
            .or_default()
            .extend(claves);
    }
    out
}

/// Los literales de cadena de un trozo de Rust, sin interpretar escapes: aquí
/// solo hay claves ASCII.
fn literales(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'"' {
                if b[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            if j < b.len() {
                out.push(s[i + 1..j].to_owned());
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn el_host_no_elige_ninguna_clave_que_no_exista() {
    let en: BTreeSet<String> = norte_i18n::message_ids(norte_i18n::Lang::En)
        .into_iter()
        .collect();
    let es: BTreeSet<String> = norte_i18n::message_ids(norte_i18n::Lang::Es)
        .into_iter()
        .collect();
    let mut faltan: Vec<String> = Vec::new();
    for (sitio, claves) in claves_elegidas() {
        for k in claves {
            if !en.contains(&k) || !es.contains(&k) {
                faltan.push(format!(
                    "{sitio}: `{k}` (en={} es={})",
                    en.contains(&k),
                    es.contains(&k)
                ));
            }
        }
    }
    faltan.sort();
    faltan.dedup();
    assert!(
        faltan.is_empty(),
        "el host elige claves que el catálogo no tiene, y el renderer las \
         PINTA tal cual:\n{}",
        faltan.join("\n")
    );
}

/// Y el test se mira a sí mismo: si deja de encontrar claves, deja de servir
/// sin decir nada.
#[test]
fn el_barrido_encuentra_algo_que_comprobar() {
    let sitios = claves_elegidas();
    assert!(
        sitios.len() > 20,
        "el barrido encontró {} sitios: o el host cambió de forma de nombrar \
         sus claves, o este test ya no mira donde están",
        sitios.len()
    );
}
