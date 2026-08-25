//! La MATRIZ DE PARIDAD de la fase 6, como test.
//!
//! El catálogo compartido es el vocabulario de comandos de norte, y la
//! ventana implementa una parte. Lo que este fichero comprueba no es que la
//! parte sea grande, sino que esté CLASIFICADA: cada comando vivo que la
//! ventana no hace está aquí abajo, o bien porque no aplica a una ventana, o
//! bien apuntado a la issue que lo va a cerrar.
//!
//! La avería que evita ya ocurrió dos veces. `pane.copy-path` estuvo dos
//! meses declarado vivo sin que lo implementara nadie —lo hacía la GUI de
//! GPUI, que se retiró—, y `task.next`/`prev`/`dismiss` igual, hasta que
//! alguien contó. El test que ata las dos mitades comprueba que todo lo del
//! TUI está en el catálogo; NO comprobaba nada en esta dirección.
//!
//! Añadir un comando al catálogo rompe este test hasta que alguien diga en
//! cuál de las dos listas cae. Eso es todo lo que hace, y es justo lo que
//! faltaba.

/// Comandos vivos que una VENTANA no va a tener, y por qué.
const NO_APLICA: &[&str] = &[
    // La línea de comandos del TUI es una superficie de terminal; la
    // respuesta de la ventana es la paleta.
    "pane.command-line",
    // `--pick` es un modo de la CLI: una ventana no tiene tubería a la que
    // contestar.
    "app.pick-accept",
    // La cierra el gestor de ventanas.
    "app.quit",
    // Una barra de menú y un «pantalla completa» son respuestas con forma de
    // terminal. La ventana no tiene barra de menú (todavía).
    "app.menu",
    "app.toggle-panels",
];

/// Comandos vivos APLAZADOS, con la issue que los cierra.
const APLAZADOS: &[(&str, u32)] = &[
    ("dialog.add", 287),
    ("dialog.back", 287),
    ("dialog.cycle-format", 287),
    ("dialog.down", 287),
    ("dialog.filter", 287),
    ("dialog.move-down", 287),
    ("dialog.move-up", 287),
    ("dialog.newer", 287),
    ("dialog.overwrite", 287),
    ("dialog.page-down", 287),
    ("dialog.page-up", 287),
    ("dialog.pane", 287),
    ("dialog.remove", 287),
    ("dialog.rename", 287),
    ("dialog.skip", 287),
    ("dialog.sort", 287),
    ("dialog.toggle-enabled", 287),
    ("dialog.up", 287),
    ("pane.connect", 290),
    ("pane.disconnect", 290),
    ("pane.edit-new", 290),
    ("pane.tree", 290),
    // El único de los siete de la ADR 0058 que sigue fuera: esta ventana no
    // sabe PINTAR un hueco de preview —caería a «kind no soportado», en
    // gris—, y abrir un hueco que solo se pinta apagado no es abrirlo.
    ("layout.preview", 291),
];

/// Todo comando vivo o lo implementa la ventana, o está clasificado.
#[test]
fn cada_comando_vivo_esta_clasificado() {
    use norte_frontend::keymap::catalogue::{CATALOGUE, Status};
    let hace: std::collections::HashSet<&str> = norte_ui_host::commands::IMPLEMENTADOS
        .iter()
        .chain(norte_ui_host::commands::IMPLEMENTADOS_VISOR.iter())
        .chain(norte_ui_host::commands::IMPLEMENTADOS_DIALOGO.iter())
        .copied()
        .collect();
    let no_aplica: std::collections::HashSet<&str> = NO_APLICA.iter().copied().collect();
    let aplazados: std::collections::HashSet<&str> = APLAZADOS.iter().map(|(c, _)| *c).collect();
    for def in CATALOGUE {
        if def.status != Status::Live {
            continue;
        }
        let clasificado =
            hace.contains(def.name) || no_aplica.contains(def.name) || aplazados.contains(def.name);
        assert!(
            clasificado,
            "`{}` está vivo en el catálogo y la ventana no lo hace: clasifícalo \
             (NO_APLICA) o apúntalo a una issue (APLAZADOS)",
            def.name
        );
    }
}

/// Y a la inversa: nada clasificado que la ventana YA haga.
///
/// Una lista de aplazados que no se limpia al construir la capacidad es una
/// lista que miente en la otra dirección, y la matriz del plan se compone de
/// ella.
#[test]
fn nada_clasificado_esta_construido() {
    let hace: std::collections::HashSet<&str> = norte_ui_host::commands::IMPLEMENTADOS
        .iter()
        .chain(norte_ui_host::commands::IMPLEMENTADOS_VISOR.iter())
        .chain(norte_ui_host::commands::IMPLEMENTADOS_DIALOGO.iter())
        .copied()
        .collect();
    for c in NO_APLICA.iter().chain(APLAZADOS.iter().map(|(c, _)| c)) {
        assert!(
            !hace.contains(c),
            "`{c}` ya lo hace la ventana: quítalo de la clasificación"
        );
    }
}

/// Toda issue de la lista es un número de verdad.
#[test]
fn todo_aplazado_tiene_issue() {
    for (c, issue) in APLAZADOS {
        assert!(*issue > 0, "`{c}` sin issue");
    }
}
