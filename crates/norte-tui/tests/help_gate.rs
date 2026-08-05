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

use norte_help::{Issue, check_commands, check_contexts, check_corpus};
use norte_tui::keymap::{COMMANDS, DIALOG_COMMANDS, Screen};

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
    "app.help",
    "app.theme",
    "app.extensions",
    "app.palette",
    "app.settings",
    "pane.ai-rename",
    "pane.semantic-search",
    "pane.open",
    "viewer.close",
    "viewer.up",
    "viewer.down",
    "viewer.page-up",
    "viewer.page-down",
    "viewer.top",
    "viewer.bottom",
    "viewer.encoding",
    "viewer.encoding-auto",
    "viewer.hex",
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
    PENDIENTES.len() <= 49,
    "la allowlist de la puerta de documentación solo puede MENGUAR: \
     documenta el comando en vez de añadirlo aquí"
);

/// Los contextos que un tema puede declarar en su front matter: uno por
/// pantalla del keymap.
///
/// El id sigue al nombre de la VARIANTE (`browse`), no al de la sección del
/// `keymap.toml` (`[pane]`): es el contexto de la ayuda, no el del keymap, y
/// el corpus ya está escrito así. H3c cablea la búsqueda real de F1.
const CONTEXTOS: &[(Screen, &str)] = &[
    (Screen::Browse, "browse"),
    (Screen::Viewer, "viewer"),
    (Screen::Dialog, "dialog"),
];

/// Ancla de compilación para [`CONTEXTOS`]: un `match` sin comodín sobre
/// [`Screen`]. Una variante nueva no compila AQUÍ, justo encima de la tabla
/// que tiene que crecer con ella (mismo idioma que `LOCALES` en
/// `norte-help`, y una sola lista en vez de dos que se desincronizan).
const _: fn(Screen) = |screen| match screen {
    Screen::Browse | Screen::Viewer | Screen::Dialog => (),
};

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
    let contextos: Vec<&str> = CONTEXTOS.iter().map(|&(_, id)| id).collect();
    let issues = check_contexts(&contextos);
    assert!(
        issues.is_empty(),
        "arregla el front matter del tema, no esta lista: los contextos los \
         define la TUI.\n{}",
        lineas(&issues)
    );

    // DEUDA REGISTRADA para H3c/H3h, no arreglable en H3a: esto comprueba
    // que ningún tema declare un contexto inventado y que dos temas no se
    // peleen por el mismo, pero NO que cada contexto tenga tema. La spec
    // pide "todo contexto conocido tiene exactamente un tema" y hoy solo
    // `panes` declara uno (`browse`): F1 en el viewer y en un diálogo no
    // abriría nada, y esta puerta calla. Falta la mitad "cero temas" en
    // `check_contexts_in` (una variante nueva de `norte_help::Issue`), y
    // hasta que exista, este test no puede pedirla.
}

/// Un hallazgo por línea, como los imprimiría `norte doctor` (H3g).
fn lineas(issues: &[Issue]) -> String {
    issues
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}
