//! Puerta de documentación (ADR 0040): todo comando que la TUI sabe
//! despachar vive en algún tema del corpus de `norte-help`.
//!
//! Es fricción DELIBERADA y permanente, la misma idea que la suite de i18n
//! que obliga a EN+ES para cada `help-cmd-*`.
//!
//! # Qué mide exactamente esta puerta
//!
//! MENCIÓN, no explicación. Un comando queda cubierto en cuanto algún tema
//! lo nombra, y nombrarlo es tan barato como añadir su id a la lista
//! `commands` del front matter: cero prosa. Lo que la puerta garantiza, y no
//! es poco, es que ningún comando pueda existir sin que NADIE lo haya mirado
//! al escribir la ayuda, y que el corpus nunca prometa un comando que no
//! existe. Que la mención sea además un párrafo útil lo decide la revisión
//! de la página, que es donde puede decidirse — ninguna aserción sabe si un
//! párrafo explica algo.
//!
//! Hasta H3h esto llevaba una allowlist encogiente: los comandos que ningún
//! tema documentaba todavía, escritos a mano y con un techo que solo podía
//! bajar. H3h la dejó en cero y la lista se borró con ella, que era el plan
//! desde el principio. Lo que queda es la puerta desnuda: un comando nuevo sin
//! página rompe la suite y NO hay dónde apuntarlo — el arreglo es escribir el
//! párrafo.
//!
//! # La otra mitad: los CONTEXTOS
//!
//! Lo mismo, en las dos direcciones, para los sitios donde el lector puede
//! estar (H3c): un tema no puede reclamar una pantalla que la TUI no tiene, y
//! una pantalla que la TUI sabe abrir no puede quedarse sin página — F1 ahí
//! abriría el índice y nadie se quejaría. El vocabulario sale de una sola
//! fuente ([`contextos`]), y su allowlist se agotó en H3h igual que la de
//! comandos: hoy todo contexto que la TUI sabe abrir tiene página.
//!
//! Aquí la puerta mide algo MÁS que una mención: reclamar un contexto es
//! decirle al lector "esto es lo que explica lo que tienes delante". Que la
//! página lo explique de verdad lo decide quien la escribe — una aprobación de
//! agente no se explica con la página de copiar — y por eso la lista de
//! pendientes lleva escrito, línea a línea, por qué cada contexto sigue ahí.

use norte_help::{Issue, check_commands, check_contexts, check_corpus};
use norte_tui::keymap::{COMMANDS, DIALOG_COMMANDS};

/// El vocabulario contra el que se cruza el corpus: TODO lo que la TUI
/// despacha, `COMMANDS` ∪ `DIALOG_COMMANDS`.
///
/// La unión y no solo `COMMANDS`, por la otra dirección del cruce. Un
/// `{{cmd:dialog.approve}}` en la página del modal de aprobación es prosa
/// legítima — el verbo existe, la TUI lo resuelve y F1 ya lo lista (#113) —
/// pero con un vocabulario recortado a `COMMANDS` saldría como
/// `UnknownCommand`, es decir "ese comando no existe", que es falso. El
/// autor solo tendría dos salidas: no documentarlo, o ensanchar el
/// vocabulario aquí. Se ensanchó, y H3h pagó la factura: los 19 verbos
/// `dialog.*` tienen página.
///
/// Se calcula (los dos listados ya están escritos a mano en `keymap.rs`, y
/// duplicarlos aquí sería una tercera copia que se desincroniza).
fn vocabulario() -> Vec<&'static str> {
    COMMANDS
        .iter()
        .copied()
        .chain(DIALOG_COMMANDS.iter().copied())
        .collect()
}

/// Los contextos que la TUI sabe abrir. UNA fuente: el vocabulario cerrado de
/// [`norte_tui::help_context::CONTEXTS`], anclado a `Modal` allí — el `match`
/// sin comodín de `modal_context` es lo que impide que un modal nuevo llegue
/// sin que alguien decida qué página lo explica.
///
/// Duplicar la lista aquí sería la tercera copia que se desincroniza (y la
/// segunda ya se desincronizó: hasta H3c esta puerta pedía un contexto
/// `dialog` que ningún modal produce). Se calcula, como [`vocabulario`], y por
/// la misma razón.
fn contextos() -> Vec<&'static str> {
    norte_tui::help_context::CONTEXTS.to_vec()
}

#[test]
fn el_corpus_que_enviamos_esta_integro() {
    // Paridad de locales, ids únicos, enlaces que resuelven y ninguna marca
    // viva escrita donde se pinta literal. No necesita vocabulario: es lo
    // único que el propio `norte-help` ya comprueba solo.
    let issues = check_corpus();
    assert!(
        issues.is_empty(),
        "el corpus tiene problemas de integridad:\n{}",
        lines(&issues)
    );
}

#[test]
fn el_corpus_no_nombra_comandos_que_no_existen() {
    // SIN allowlist (`&[]`, literalmente), y no es una omisión: un tema que
    // nombra un comando inexistente es siempre un bug — prosa que promete
    // una tecla que no hace nada, o un id mal escrito. No hay deuda que
    // tapar aquí, solo erratas que arreglar — y desde H3h tampoco queda
    // allowlist que pasar en la otra dirección.
    let desconocidos: Vec<Issue> = check_commands(&vocabulario(), &[])
        .into_iter()
        .filter(|i| matches!(i, Issue::UnknownCommand { .. }))
        .collect();
    assert!(
        desconocidos.is_empty(),
        "el corpus nombra comandos fuera del vocabulario de la TUI:\n{}",
        lines(&desconocidos)
    );
}

#[test]
fn todo_comando_del_vocabulario_esta_documentado() {
    // `&[]` y no una allowlist: desde H3h no hay deuda que tapar. Un comando
    // sin página es un fallo con un solo arreglo — escribir el párrafo — y no
    // existe la línea que lo aplazaría.
    let issues = check_commands(&vocabulario(), &[]);

    let sin_documentar: Vec<&Issue> = issues
        .iter()
        .filter(|i| matches!(i, Issue::UndocumentedCommand { .. }))
        .collect();
    assert!(
        sin_documentar.is_empty(),
        "comandos que ningún tema documenta. Escribe el párrafo: la \
         allowlist que aplazaba esto se agotó en H3h y no va a volver.\n{}",
        sin_documentar
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );

    // Y nada más. `check_commands` puede crecer con variantes nuevas: que
    // una aparezca y este fichero la ignore en silencio sería exactamente
    // el fallo que la puerta existe para no tener.
    assert!(
        issues.is_empty(),
        "hallazgos que esta puerta no clasifica:\n{}",
        lines(&issues)
    );
}

#[test]
fn los_contextos_del_corpus_son_pantallas_que_la_tui_tiene() {
    let contextos = contextos();
    let issues = check_contexts(&contextos);

    // Una dirección: ningún tema declara un contexto inventado, y dos temas
    // no se pelean por el mismo. Aquí la lista no se toca — el arreglo está
    // en el front matter del tema, porque los contextos los define la TUI.
    let del_corpus: Vec<&Issue> = issues
        .iter()
        .filter(|i| {
            matches!(
                i,
                Issue::UnknownContext { .. } | Issue::DuplicateContext { .. }
            )
        })
        .collect();
    assert!(
        del_corpus.is_empty(),
        "arregla el front matter del tema, no esta lista: los contextos los \
         define la TUI.\n{}",
        del_corpus
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );

    // La otra: cada contexto que la TUI sabe abrir tiene una página. Sin
    // esto, F1 en una pantalla sin página abre el índice y nadie se queja.
    let sin_pagina: Vec<&str> = issues
        .iter()
        .filter_map(|i| match i {
            Issue::ContextWithoutTopic { context, .. } => Some(context.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        sin_pagina.is_empty(),
        "contextos sin página. Reclámalos desde el `context` de la página que \
         los explica, o escribe esa página: la allowlist que los aplazaba se \
         agotó en H3h. {sin_pagina:?}"
    );

    // Y nada más, por lo mismo que en la puerta de comandos: una variante
    // nueva de `Issue` que este fichero ignorase en silencio sería
    // exactamente el fallo que la puerta existe para no tener.
    let sin_clasificar: Vec<&Issue> = issues
        .iter()
        .filter(|i| {
            !matches!(
                i,
                Issue::UnknownContext { .. }
                    | Issue::DuplicateContext { .. }
                    | Issue::ContextWithoutTopic { .. }
            )
        })
        .collect();
    assert!(
        sin_clasificar.is_empty(),
        "hallazgos que esta puerta no clasifica:\n{}",
        sin_clasificar
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Un hallazgo por línea, como los imprimiría `norte doctor` (H3g).
fn lines(issues: &[Issue]) -> String {
    issues
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}
