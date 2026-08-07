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
//! [`PENDIENTES`] es la deuda visible mientras H3h redacta el corpus
//! completo. Está escrita a mano, un comando por línea, para que el `git
//! diff` de cada tema muestre exactamente qué se saldó, y solo puede MENGUAR
//! porque hay dos mecanismos, no una promesa: `check_commands` reporta las
//! entradas que dejaron de tapar algo ([`norte_help::Stale`]), y el
//! `const _` de debajo de la lista impide que crezca sin que alguien suba el
//! techo a mano en el mismo diff.
//!
//! # La otra mitad: los CONTEXTOS
//!
//! Lo mismo, en las dos direcciones, para los sitios donde el lector puede
//! estar (H3c): un tema no puede reclamar una pantalla que la TUI no tiene, y
//! una pantalla que la TUI sabe abrir no puede quedarse sin página — F1 ahí
//! abriría el índice y nadie se quejaría. El vocabulario sale de una sola
//! fuente ([`contextos`]) y la deuda de [`CONTEXTOS_PENDIENTES`] tiene los
//! mismos dos mecanismos de menguado que [`PENDIENTES`].
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
/// vocabulario aquí. Mejor ensancharlo YA, con los 19 verbos `dialog.*`
/// entrando en [`PENDIENTES`] como la deuda que son.
///
/// Se calcula (los dos listados ya están escritos a mano en `keymap.rs`, y
/// duplicarlos aquí sería una tercera copia que se desincroniza). La
/// allowlist, en cambio, es literal: ver [`PENDIENTES`].
fn vocabulario() -> Vec<&'static str> {
    COMMANDS
        .iter()
        .copied()
        .chain(DIALOG_COMMANDS.iter().copied())
        .collect()
}

/// Comandos que ningún tema documenta todavía. Se borran, uno a uno,
/// conforme H3h escribe las páginas.
///
/// A MANO y en orden de vocabulario (primero `COMMANDS`, después
/// `DIALOG_COMMANDS`), jamás calculada: una allowlist derivada del propio
/// corpus taparía cualquier regresión futura por construcción, que es
/// justamente lo contrario de una puerta.
const PENDIENTES: &[&str] = &[
    "app.quit",
    "cursor.up",
    "cursor.down",
    "cursor.page-up",
    "cursor.page-down",
    "cursor.top",
    "cursor.bottom",
    "app.theme",
    "app.extensions",
    "app.settings",
    "pane.ai-rename",
    "pane.semantic-search",
    "pane.quick-search",
    "pane.search",
    "pane.toggle-hidden",
    "pane.columns",
    "pane.mkdir",
    "dialog.confirm",
    "dialog.cancel",
    "dialog.approve",
    "dialog.deny",
    "dialog.overwrite",
    "dialog.skip",
    "dialog.rename",
    "dialog.newer",
    "dialog.up",
    "dialog.down",
    "dialog.page-up",
    "dialog.page-down",
    "dialog.add",
    "dialog.toggle-enabled",
    "dialog.remove",
    "dialog.move-up",
    "dialog.move-down",
    "dialog.sort",
    "dialog.cycle-format",
];

/// El TECHO de la deuda: la puerta no solo obliga a que la lista mengüe
/// (eso ya lo vigila `StaleAllowEntry`), sino que impide que CREZCA.
///
/// Sin esto, "un comando nuevo cuesta un párrafo" sería falso: se saldaría
/// con una línea aquí y nadie se enteraría. Con esto, subir el techo es una
/// edición deliberada, en el mismo diff, que un revisor ve. El número solo
/// puede bajar — y cuando H3h lo deje en 0, la lista desaparece con él.
const _: () = assert!(
    PENDIENTES.len() <= 36,
    "la allowlist de la puerta de documentación solo puede MENGUAR: \
     documenta el comando en vez de añadirlo aquí"
);

/// Los contextos que la TUI sabe abrir. UNA fuente: el vocabulario cerrado de
/// [`norte_tui::help_context::CONTEXTS`], anclado a `Modal` allí — el `match`
/// sin comodín de `modal_context` es lo que impide que un modal nuevo llegue
/// sin que alguien decida qué página lo explica.
///
/// Duplicar la lista aquí sería la tercera copia que se desincroniza (y la
/// segunda ya se desincronizó: hasta H3c esta puerta pedía un contexto
/// `dialog` que ningún modal produce). Se calcula, como [`vocabulario`], y por
/// la misma razón; la allowlist, en cambio, es literal.
fn contextos() -> Vec<&'static str> {
    norte_tui::help_context::CONTEXTS.to_vec()
}

/// Contextos que todavía no tienen página. Se borran, uno a uno, conforme H3h
/// escribe el corpus.
///
/// A MANO y en orden de vocabulario, jamás calculada, por lo mismo que
/// [`PENDIENTES`]: una lista derivada del propio corpus taparía la regresión
/// por construcción.
///
/// Y solo puede MENGUAR por dos mecanismos, no por una promesa: el `const _`
/// de debajo impide que crezca sin subir el techo a mano en el mismo diff, y
/// el test de abajo reporta como fallo la entrada que ya no tapa nada —
/// alguien escribió la página y se dejó la línea, silenciando al siguiente
/// contexto que caiga ahí.
const CONTEXTOS_PENDIENTES: &[&str] = &[
    // Ni página de agentes y política: también H3h. Ningún tema habla hoy de
    // aprobaciones, y hacer que `copying` reclame este contexto para callar
    // la puerta sería contarle al lector lo que no ha preguntado.
    "dialog.approval",
    // El TOFU del `init.lua` de un proyecto: ningún tema menciona ni los
    // plugins ni el `init.lua`.
    "dialog.trust-lua",
    // Salir: `app.quit` sigue en PENDIENTES, así que tampoco hay prosa que
    // explique la pregunta.
    "dialog.quit",
    // El nombre editable de una transferencia (y el renombrado, que abre el
    // mismo modal): `copying` cuenta que el destino es el otro panel, no que
    // se pueda teclear el nombre, y `mouse` solo NOMBRA `pane.rename` en la
    // lista del menú contextual.
    "dialog.transfer-name",
    // Crear directorio: `pane.mkdir` está en PENDIENTES.
    "dialog.mkdir",
    // Renombrado por IA: `pane.ai-rename` está en PENDIENTES.
    "dialog.ai-rename",
    // Búsqueda semántica: `pane.semantic-search` está en PENDIENTES.
    "dialog.semantic-search",
];

/// El TECHO de la deuda de contextos, con el mismo papel que el de
/// [`PENDIENTES`]: la lista solo puede bajar, y subir el número es una
/// edición deliberada que un revisor ve en el mismo diff.
const _: () = assert!(
    CONTEXTOS_PENDIENTES.len() <= 7,
    "la allowlist de contextos solo puede MENGUAR: escribe la página en vez \
     de añadir el contexto aquí"
);

#[test]
fn el_corpus_que_enviamos_esta_integro() {
    // Paridad de locales, ids únicos, enlaces que resuelven y ninguna marca
    // viva escrita donde se pinta literal. No necesita vocabulario: es lo
    // único que el propio `norte-help` ya comprueba solo.
    let issues = check_corpus();
    assert!(
        issues.is_empty(),
        "el corpus tiene problemas de integridad:\n{}",
        lineas(&issues)
    );
}

#[test]
fn el_corpus_no_nombra_comandos_que_no_existen() {
    // SIN allowlist (`&[]`, literalmente), y no es una omisión: un tema que
    // nombra un comando inexistente es siempre un bug — prosa que promete
    // una tecla que no hace nada, o un id mal escrito. No hay deuda que
    // tapar aquí, solo erratas que arreglar. Pasar `PENDIENTES` daría el
    // mismo resultado hoy (la allowlist no toca esta dirección del cruce),
    // pero diría lo contrario de lo que este test afirma.
    let desconocidos: Vec<Issue> = check_commands(&vocabulario(), &[])
        .into_iter()
        .filter(|i| matches!(i, Issue::UnknownCommand { .. }))
        .collect();
    assert!(
        desconocidos.is_empty(),
        "el corpus nombra comandos fuera del vocabulario de la TUI:\n{}",
        lineas(&desconocidos)
    );
}

#[test]
fn todo_comando_del_vocabulario_esta_documentado_o_en_pendientes() {
    let issues = check_commands(&vocabulario(), PENDIENTES);

    let sin_documentar: Vec<&Issue> = issues
        .iter()
        .filter(|i| matches!(i, Issue::UndocumentedCommand { .. }))
        .collect();
    assert!(
        sin_documentar.is_empty(),
        "comandos sin tema y sin entrada en PENDIENTES. Escribe el párrafo, \
         o añade el comando a la lista si toca esperar a H3h:\n{}",
        sin_documentar
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );

    // La otra mitad de la puerta: una entrada que ya no tapa nada. Sin
    // esto la allowlist deja de menguar y nadie se entera — el comando se
    // documentó (o se renombró) y la línea sigue ahí, silenciando el
    // siguiente comando que se llame igual.
    let rancias: Vec<&Issue> = issues
        .iter()
        .filter(|i| matches!(i, Issue::StaleAllowEntry { .. }))
        .collect();
    assert!(
        rancias.is_empty(),
        "entradas de PENDIENTES que ya no tapan nada; bórralas:\n{}",
        rancias
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
        lineas(&issues)
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

    // La otra: cada contexto que la TUI sabe abrir tiene una página, o está
    // enumerado en CONTEXTOS_PENDIENTES. Sin esto, F1 en una pantalla sin
    // página abre el índice y nadie se queja.
    let sin_pagina: Vec<&str> = issues
        .iter()
        .filter_map(|i| match i {
            Issue::ContextWithoutTopic { context, .. } => Some(context.as_str()),
            _ => None,
        })
        .collect();
    let sin_tapar: Vec<&&str> = sin_pagina
        .iter()
        .filter(|c| !CONTEXTOS_PENDIENTES.contains(c))
        .collect();
    assert!(
        sin_tapar.is_empty(),
        "contextos sin página y sin entrada en CONTEXTOS_PENDIENTES. \
         Reclámalos desde el `context` de la página que ya los explica, o \
         añádelos a la lista si toca esperar a H3h: {sin_tapar:?}"
    );

    // Y la mitad que hace que la allowlist mengüe de verdad: una entrada que
    // ya no tapa nada. O bien la página se escribió y la línea sobrevivió —
    // silenciando al siguiente contexto que caiga ahí — o bien el id salió
    // del vocabulario y la línea no silencia nada.
    let rancias: Vec<String> = CONTEXTOS_PENDIENTES
        .iter()
        .filter(|c| !sin_pagina.contains(*c))
        .map(|c| {
            if contextos.contains(c) {
                format!("`{c}` ya tiene página: borra la línea")
            } else {
                format!("`{c}` ya no está en el vocabulario de la TUI: borra la línea")
            }
        })
        .collect();
    assert!(
        rancias.is_empty(),
        "entradas de CONTEXTOS_PENDIENTES que ya no tapan nada:\n{}",
        rancias.join("\n")
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
fn lineas(issues: &[Issue]) -> String {
    issues
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}
