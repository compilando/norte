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
    // «Pantalla completa» es una respuesta con forma de terminal: esconder
    // los paneles para ver lo que hay detrás no significa nada en una ventana
    // que ES el gestor.
    //
    // `app.menu` estuvo aquí y ya NO: la ventana tiene barra de menús, con
    // los mismos menús y las mismas entradas que el TUI, porque el modelo es
    // `norte_frontend::menu` y no una copia.
    "app.toggle-panels",
];

/// Comandos vivos APLAZADOS, con la issue que los cierra.
const APLAZADOS: &[(&str, u32)] = &[
    // El resto de `dialog.*` YA pasa por el resolutor compartido (#287). Este
    // no: quitar una fila de una lista solo significa algo sobre una lista que
    // se pueda EDITAR, y la única de esta ventana —los ajustes— es de solo
    // lectura. Atarlo a algo ahora sería inventarle una superficie.
    ("dialog.remove", 287),
    // El único de los siete de la ADR 0058 que sigue fuera: esta ventana no
    // sabe PINTAR un hueco de preview —caería a «kind no soportado», en
    // gris—, y abrir un hueco que solo se pinta apagado no es abrirlo.
    ("layout.preview", 291),
    // El renombrado en lote por plantilla (#310) nace en la TUI: el generador
    // y la validación viven en el crate COMPARTIDO, así que lo que le falta a
    // la ventana es la superficie —un prompt de plantilla— y no la lógica. La
    // revisión del plan sí la tiene ya, porque es la misma que la del rename
    // con IA.
    ("pane.rename-batch", 310),
    // Las sumas (#311) nacen igual: el protocolo, el core y el parser del
    // fichero de sumas son compartidos, y lo que la ventana no tiene todavía
    // es dónde ENSEÑAR la lista —una tabla con su veredicto por fila— ni el
    // gesto de copiarla al portapapeles.
    ("pane.checksum", 311),
    ("pane.checksum-verify", 311),
    // Comparar dos ficheros (#312) delega en un programa externo, y esta
    // ventana todavía no sabe lanzar uno esperándolo: el camino nativo que
    // tiene —`shell::open`— es el de «entrégaselo al escritorio y vuelve»,
    // que para un `diff -u` de terminal es un parpadeo. La regla del operando
    // sí es compartida (`norte_frontend::diffpair`), así que lo que falta es
    // el lanzamiento, no la decisión.
    ("pane.compare-files", 312),
    // Cambiar permisos (#314) nace en la TUI. El protocolo, el journal con su
    // reversa y la lectura del modo son compartidos; lo que le falta a la
    // ventana es el diálogo —un campo de cuatro dígitos octales con la cuenta
    // de sobre cuántas entradas va— y no la decisión.
    ("pane.chmod", 314),
    // Los perfiles (ADR 0079) ya están en las dos: el selector, girar por la
    // lista y el cambio en caliente. Lo que la ventana todavía no hace es
    // acordarse de dónde dejaste cada panel DENTRO de cada perfil — la
    // disposición sale de la configuración del perfil, no de su estado
    // guardado— y eso es lo único que queda de #307.
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
