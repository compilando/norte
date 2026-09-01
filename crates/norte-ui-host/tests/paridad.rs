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
    // `dialog.remove` estuvo aquí con un motivo que era cierto: quitar una
    // fila solo significa algo sobre una lista que se pueda EDITAR, y la única
    // de esta ventana era de solo lectura. Desde #309 hay una que sí —los
    // favoritos—, así que se fue de esta lista y entró en
    // `IMPLEMENTADOS_DIALOGO`.
    // El único de los siete de la ADR 0058 que sigue fuera: esta ventana no
    // sabe PINTAR un hueco de preview —caería a «kind no soportado», en
    // gris—, y abrir un hueco que solo se pinta apagado no es abrirlo.
    ("layout.preview", 291),
    // El panel de registro (#323) nace en la TUI, y lo COMPARTIDO ya está
    // hecho: el anillo y su capa en `norte-config`, el estado de presentación
    // —filtros, seguimiento del final— en `norte-frontend`. Lo que le falta a
    // la ventana es pintarlo y montar el anillo al arrancar. Con un matiz que
    // no tiene la TUI: la ventana arranca su propio daemon (#300), así que su
    // anillo llevaría lo de ESTE proceso y no lo del daemon, que es donde pasa
    // la mitad interesante.
    ("layout.log", 326),
    // El renombrado en lote por plantilla (#310) nace en la TUI: el generador
    // y la validación viven en el crate COMPARTIDO, así que lo que le falta a
    // la ventana es la superficie —un prompt de plantilla— y no la lógica. La
    // revisión del plan sí la tiene ya, porque es la misma que la del rename
    // con IA.
    ("pane.rename-batch", 310),
    // Comparar dos ficheros (#312) delega en un programa externo, y esta
    // ventana todavía no sabe lanzar uno esperándolo: el camino nativo que
    // tiene —`shell::open`— es el de «entrégaselo al escritorio y vuelve»,
    // que para un `diff -u` de terminal es un parpadeo. La regla del operando
    // sí es compartida (`norte_frontend::diffpair`), así que lo que falta es
    // el lanzamiento, no la decisión.
    ("pane.compare-files", 312),
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

/// Los DOS frontends preguntan la contraseña; ninguno se queda pintando el
/// error (#325/#327).
///
/// No es un comando, así que no cae en las listas de arriba — y por eso mismo
/// se le pone una prueba propia. Es una DECISIÓN duplicada entre frontends, que
/// es la clase de cosa que ADR 0077 existe para que no diverja en silencio: la
/// TUI la tomó en #325 y la ventana tardó dos versiones en tomarla, durante las
/// cuales un usuario de `norte-gui` sobre una conexión `secret = "prompt"` leía
/// el nombre de una variable de entorno y se quedaba ahí.
///
/// Se comprueba por el CÓDIGO y no por comportamiento porque son dos binarios
/// con dos bucles distintos; lo que esta prueba impide es que alguien borre el
/// brazo de uno de los dos y el otro siga verde.
#[test]
fn los_dos_frontends_preguntan_el_secreto() {
    let sitios = [
        // La TUI: el `cd` que se topa con el error abre su modal.
        ("norte-tui", "../norte-tui/src/navigate.rs"),
        // La ventana: el listado que vuelve con el error abre su diálogo.
        ("norte-ui-host", "src/controller/listing.rs"),
    ];
    for (quien, ruta) in sitios {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(ruta);
        let src = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        assert!(
            src.contains("Error::SecretNeeded"),
            "{quien} ya no reacciona a `SecretNeeded` en {ruta}: o lo movió, o \
             volvió a dejar al lector delante de un error que no puede contestar"
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
