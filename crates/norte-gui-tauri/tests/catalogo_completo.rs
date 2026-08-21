//! Toda clave Fluent que el renderer pide EXISTE en el catálogo.
//!
//! `Screen::t` contesta una clave que no está con la clave misma, así que una
//! que falte no se cae: se PINTA. Se descubrió con `hostile-name`, que no
//! existía y llevaba tiempo saliendo literal en la insignia de todo nombre
//! alterado — en ocho superficies, y sin que ningún test dijera nada, porque
//! el catálogo de los tests del renderer es un fixture que se la inventaba.
//!
//! El test lee el TypeScript, que es la única fuente de verdad de qué pide
//! quien pinta: una lista escrita a mano se separaría de él en la primera
//! superficie nueva.

use std::collections::BTreeSet;

/// Las claves que `render.ts` le pide al catálogo.
fn claves_pedidas() -> BTreeSet<String> {
    let fuente = include_str!("../ui/src/render.ts");
    let mut out = BTreeSet::new();
    let mut resto = fuente;
    // `this.t("clave")` — literales, que es como se piden todas menos las
    // compuestas, que se tratan aparte abajo.
    while let Some(i) = resto.find("this.t(\"") {
        resto = &resto[i + "this.t(\"".len()..];
        if let Some(fin) = resto.find('"') {
            out.insert(resto[..fin].to_owned());
        }
    }
    out
}

/// Las que se componen con un sufijo variable, con sus valores posibles.
///
/// A mano y con su valor: son las únicas que un `grep` no puede resolver, y
/// dejarlas fuera sería el mismo agujero que este test viene a tapar.
const COMPUESTAS: &[(&str, &[&str])] = &[("help-callout-", &["note", "warn", "tip"])];

#[test]
fn el_renderer_no_pide_ninguna_clave_que_no_exista() {
    let existentes: BTreeSet<String> = [norte_i18n::Lang::En, norte_i18n::Lang::Es]
        .into_iter()
        .flat_map(norte_i18n::message_ids)
        .collect();

    let mut faltan: Vec<String> = claves_pedidas()
        .into_iter()
        .filter(|k| !existentes.contains(k))
        .collect();
    for (prefijo, sufijos) in COMPUESTAS {
        for s in *sufijos {
            let clave = format!("{prefijo}{s}");
            if !existentes.contains(&clave) {
                faltan.push(clave);
            }
        }
    }
    assert!(
        faltan.is_empty(),
        "el renderer pinta estas claves tal cual, porque no están en el \
         catálogo: {faltan:?}"
    );
}

/// Y las dos mitades del catálogo dicen lo mismo: una clave que solo está en
/// un idioma es una ventana que habla en dos.
#[test]
fn los_dos_idiomas_tienen_las_mismas_claves() {
    let en: BTreeSet<String> = norte_i18n::message_ids(norte_i18n::Lang::En)
        .into_iter()
        .collect();
    let es: BTreeSet<String> = norte_i18n::message_ids(norte_i18n::Lang::Es)
        .into_iter()
        .collect();
    let falta_en_castellano: Vec<&String> = en.difference(&es).collect();
    let falta_en_ingles: Vec<&String> = es.difference(&en).collect();
    assert!(
        falta_en_castellano.is_empty() && falta_en_ingles.is_empty(),
        "solo en inglés: {falta_en_castellano:?}; solo en español: {falta_en_ingles:?}"
    );
}
