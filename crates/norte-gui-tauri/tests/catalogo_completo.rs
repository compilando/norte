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

/// Los ficheros del renderer que piden claves.
///
/// `main.ts` también: llama a `screen.t(...)`, y mirar solo `render.ts`
/// dejaba fuera todo lo que se pinta desde el arranque.
const FUENTES: &[(&str, &str)] = &[
    ("render.ts", include_str!("../ui/src/render.ts")),
    ("main.ts", include_str!("../ui/src/main.ts")),
];

/// Las formas de pedir una clave. Las TRES, no una.
///
/// El barrido miraba solo `this.t("` con comilla doble, así que no veía la
/// función libre `tr(...)` ni las plantillas. Por ahí se coló `task-foreign`,
/// que no existe en ningún catálogo y que este test daba por verde.
const LLAMADAS: &[&str] = &["this.t(", "screen.t(", "tr("];

/// Las claves que el renderer le pide al catálogo.
///
/// Un sitio de llamada cuyo argumento NO es un literal se exige que esté
/// registrado en [`COMPUESTAS`]: ignorarlo en silencio es lo que dejaba pasar
/// las plantillas.
fn claves_pedidas() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (nombre, fuente) in FUENTES {
        for llamada in LLAMADAS {
            let mut desde = 0usize;
            while let Some(i) = fuente[desde..].find(llamada) {
                let inicio = desde + i + llamada.len();
                desde = inicio;
                // El ARGUMENTO entero, hasta el paréntesis que cierra: un
                // `t(a ? "x" : "y")` pide DOS claves, y quedarse con la
                // primera —o con ninguna— es el agujero de siempre.
                let arg = argumento(&fuente[inicio..]);
                let literales = comillas(arg);
                if !literales.is_empty() {
                    out.extend(literales);
                    continue;
                }
                // Una plantilla o una variable: solo vale si está declarada
                // como compuesta, o si la clave la elige RUST —y entonces la
                // comprueba `norte-ui-host/tests/catalogo_del_host.rs`, que
                // lee el código del host por el mismo motivo que este lee el
                // del renderer.
                assert!(
                    COMPUESTAS.iter().any(|(p, _)| arg.contains(p))
                        || DEL_HOST.iter().any(|v| arg.starts_with(v)),
                    "{nombre}: `{llamada}{arg}` pide una clave que este test \
                     no puede resolver y que no está en `COMPUESTAS`. Una \
                     clave que el barrido no ve se pinta como su propio \
                     identificador el día que falte, y eso es exactamente lo \
                     que este test viene a impedir."
                );
            }
        }
    }
    out
}

/// El argumento de una llamada, desde justo tras su `(` hasta el `)` que la
/// cierra.
fn argumento(resto: &str) -> &str {
    let mut nivel = 1i32;
    for (i, c) in resto.char_indices() {
        match c {
            '(' => nivel += 1,
            ')' => {
                nivel -= 1;
                if nivel == 0 {
                    return &resto[..i];
                }
            }
            _ => {}
        }
    }
    resto
}

/// Los literales entre comillas dobles de un trozo de TypeScript.
fn comillas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut resto = s;
    while let Some(i) = resto.find('"') {
        resto = &resto[i + 1..];
        let Some(fin) = resto.find('"') else {
            break;
        };
        out.push(resto[..fin].to_owned());
        resto = &resto[fin + 1..];
    }
    out
}

/// Los argumentos cuya clave la ELIGE el host, en Rust.
///
/// No es una excepción: es un traspaso. Estas claves las comprueba
/// `norte-ui-host/tests/catalogo_del_host.rs` leyendo el código del host,
/// igual que este test lee el del renderer. Nombrarlas aquí obliga a que
/// añadir una forma nueva de recibir una clave desde el host pase por los dos
/// ficheros; el barrido anterior simplemente no las veía.
const DEL_HOST: &[&str] = &[
    "ack.reason_key",
    "out.notice.key",
    "slot.state.reason_key",
    "top.title_key",
    // El título del panel de salida de un programa (#312): el host la elige
    // entre literales suyos, que el barrido del host sí sigue.
    "output.title_key",
    "c.label_key",
    // `taskNode` recibe el traductor y compone `gui-task-kind-…`, que está
    // en `COMPUESTAS`.
    "k",
];

/// Las que se componen con un sufijo variable, con sus valores posibles.
///
/// A mano y con su valor: son las únicas que un `grep` no puede resolver, y
/// dejarlas fuera sería el mismo agujero que este test viene a tapar.
const COMPUESTAS: &[(&str, &[&str])] = &[
    ("help-callout-", &["note", "warn", "tip"]),
    // Los mandos de nivel del panel de registro (#326). El sufijo es el
    // vocabulario CERRADO de `LogLevel::wire`, y esta lista es la otra mitad:
    // un nivel nuevo allí rompe aquí, que es donde hay que enterarse de que
    // le falta su cadena.
    ("log-level-", &["error", "warn", "info", "debug", "trace"]),
    // El sufijo es `TaskView::kind`, que lo produce `clase_de_task` en el
    // host con un `match` exhaustivo: esta lista es la otra mitad de ese
    // `match`, y una variante nueva de `TaskKind` rompe allí primero.
    (
        "gui-task-kind-",
        &[
            "copy",
            "move",
            "delete",
            "undo",
            "search",
            "mkdir",
            "index",
            "embed",
            "rename-batch",
            "compare",
            "dir-size",
            "pack",
            "test-archive",
            "split",
            "combine",
            "sync-plan",
            "sync",
            "unknown",
        ],
    ),
];

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
