//! Qué comandos sabe ejecutar este host, y qué hace con los que no.
//!
//! La lista importa por dos motivos. Uno: es lo que el keymap efectivo
//! necesita para decidir si una tecla ligada puede ejecutarse AQUÍ
//! (`Availability::NotHere` es «este frontend no lo implementa», y sin la
//! lista no se puede distinguir de «norte no lo ha construido»). Y dos: es
//! la única declaración honesta de hasta dónde llega el host, en vez de un
//! `match` que se traga en silencio lo que no reconoce.

/// Hasta dónde llega un frontend: si puede MUTAR o solo mirar.
///
/// No es una amputación del host —el host sabe borrar y crear, y sus tests lo
/// prueban— sino una decisión de ARRANQUE de quien lo monta. La ventana
/// gráfica arranca en solo lectura hasta que la fase 5 le dé el camino seguro
/// (el gate de salida de la fase 4 lo exige), y hasta entonces una tecla
/// atada a `pane.delete` en el preset se responde en vez de ejecutarse: que
/// la tecla exista no es permiso.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Efectos {
    /// Solo mirar: navegar, marcar, ordenar, ver. Nada que escriba, y
    /// tampoco aprobar que escriba un agente.
    SoloLectura,
    /// Todo lo que el host implementa.
    Completo,
}

/// Los comandos que el host ejecuta en cada modo.
///
/// La lista de solo lectura es la de siempre MENOS lo que no es inerte: lo
/// que escribe, borra, lanza un programa ajeno, lee contenido entero o manda
/// datos fuera del proceso. Ese juicio NO se hace aquí: es el `effect` que el
/// catálogo declara en cada fila, sin valor por defecto (ADR 0126). Antes era
/// una lista propia, `MUTAN`, y olvidarse de ella al añadir un comando que
/// escribe dejaba a la ventana de «solo mirar» ejecutándolo.
#[must_use]
pub fn implementados(efectos: Efectos) -> Vec<&'static str> {
    match efectos {
        Efectos::Completo => IMPLEMENTADOS.to_vec(),
        Efectos::SoloLectura => IMPLEMENTADOS
            .iter()
            .copied()
            .filter(|c| inerte(c))
            .collect(),
    }
}

/// Si el catálogo declara `command` inerte. Un nombre que el catálogo no
/// conoce NO lo es: no saber qué hace no autoriza a ejecutarlo.
fn inerte(command: &str) -> bool {
    norte_frontend::keymap::catalogue::effect(command)
        .is_some_and(norte_frontend::keymap::Effect::is_inert)
}

/// Los comandos que el host ejecuta HOY.
///
/// Crece con cada tarea de la fase 2. Todo lo demás del catálogo resuelve a
/// [`norte_frontend::keymap::Availability::NotHere`] y se DICE en la barra,
/// que es exactamente lo que hace el TUI con los suyos.
pub const IMPLEMENTADOS: &[&str] = &[
    "cursor.up",
    "cursor.down",
    "cursor.page-up",
    "cursor.page-down",
    "cursor.top",
    "cursor.bottom",
    "nav.enter",
    "nav.parent",
    "nav.back",
    "nav.forward",
    "nav.jump-back",
    "nav.set-jump-point",
    "mark.toggle",
    "mark.clear",
    "mark.all",
    "mark.invert",
    "mark.pattern-add",
    "mark.pattern-remove",
    "mark.extension-add",
    "mark.extension-remove",
    "mark.files",
    "mark.dirs",
    "mark.restore",
    "mark.toggle-up",
    "mark.toggle-page-down",
    "mark.toggle-page-up",
    "mark.to-top",
    "mark.to-bottom",
    "pane.switch",
    "layout.focus-next",
    "layout.focus-prev",
    "layout.set-target",
    "layout.grow",
    "layout.shrink",
    "layout.equalize",
    "layout.flip",
    "layout.pick",
    "layout.split-h",
    "layout.split-v",
    "layout.close-slot",
    "layout.places",
    "layout.processes",
    "layout.log",
    "layout.disk-map",
    "layout.timeline",
    "layout.metadata",
    "layout.preview",
    "pane.tree",
    "pane.tab-new",
    "pane.tab-close",
    "pane.tab-next",
    "pane.tab-prev",
    "pane.tab-move-left",
    "pane.tab-move-right",
    "pane.tab-goto-1",
    "pane.tab-goto-2",
    "pane.tab-goto-3",
    "pane.tab-goto-4",
    "pane.tab-goto-5",
    "pane.tab-goto-6",
    "pane.tab-goto-7",
    "pane.tab-goto-8",
    "pane.tab-goto-9",
    "pane.columns",
    "app.palette",
    "app.goto",
    "app.help",
    "app.settings",
    "app.extensions",
    "app.agents",
    "app.terminal",
    "app.handoff",
    "pane.open",
    "pane.compare-files",
    "pane.edit",
    "pane.copy-path",
    "app.theme",
    "app.menu",
    // Salir por la tecla, como en el terminal: la ventana la tenía como
    // «la cierra el gestor de ventanas», y `F10`, `q` y «Salir» del menú no
    // hacían nada. Va por el mismo camino que el botón de cerrar.
    "app.quit",
    "profile.pick",
    "profile.save-as",
    "profile.next",
    "profile.prev",
    "pane.select-drive",
    "pane.connect",
    "pane.disconnect",
    "pane.view",
    "pane.quick-search",
    "pane.search",
    "pane.mkdir",
    "pane.edit-new",
    "pane.delete",
    "pane.delete-permanent",
    "pane.copy",
    "pane.move",
    "pane.rename",
    "pane.chmod",
    "pane.checksum",
    "pane.checksum-verify",
    "pane.ai-rename",
    "pane.organize",
    "pane.rename-batch",
    "pane.semantic-search",
    "pane.compare-dirs",
    "pane.sync-dirs",
    // #290 fase A: los gestos de panel que el TUI tenía y la ventana no.
    // Ninguno escribe ni saca datos del proceso, así que el catálogo los
    // declara inertes: re-listar es lo mismo que ya hace navegar.
    "pane.sort-name",
    "pane.sort-ext",
    "pane.sort-size",
    "pane.sort-time",
    "pane.sort-menu",
    "pane.refresh",
    "pane.toggle-hidden",
    "pane.names-encoding",
    "pane.properties",
    "pane.dir-size",
    "pane.pack",
    "pane.unpack",
    "pane.test-archive",
    "pane.split-file",
    "pane.combine-files",
    "pane.mirror",
    "pane.mirror-target",
    "pane.sync-nav",
    "pane.pull",
    "pane.swap",
    "pane.history",
    "pane.hotlist",
    "pane.popular",
    "pane.history-left",
    "pane.history-right",
    "pane.select-drive-left",
    "pane.select-drive-right",
    "task.cancel",
    "task.next",
    "task.prev",
    "task.dismiss",
];

/// Los comandos de la pantalla del VISOR que el host ejecuta.
///
/// Lista aparte porque es otra pantalla, y su keymap efectivo se construye
/// con `Screen::Viewer`: un comando que no esté aquí resuelve a
/// [`norte_frontend::keymap::Availability::NotHere`] y se DICE, igual que en
/// el listado.
/// Los verbos de DIÁLOGO que este host atiende.
///
/// Cuatro y no los veintidós del catálogo: los diálogos de esta ventana son
/// preguntas con dos respuestas —confirmar/cancelar, aprobar/denegar—, y los
/// demás verbos (`dialog.overwrite`, `dialog.sort`, `dialog.pane`…) nombran
/// respuestas de diálogos que aquí no existen. Un preset puede atarlos: la
/// tecla dirá que aquí no, con la misma frase que cualquier otro comando que
/// esta ventana no hace.
pub const IMPLEMENTADOS_DIALOGO: &[&str] = &[
    "dialog.confirm",
    "dialog.cancel",
    "dialog.approve",
    "dialog.deny",
    // Las cuatro salidas de una colisión (#287). Cada una nombra SU
    // respuesta: «confirmar» no dice cuál de las cuatro.
    "dialog.overwrite",
    "dialog.skip",
    "dialog.rename",
    "dialog.newer",
    // Andar por una lista modal, y sus dos extremos (`dialog.top`/`bottom`).
    "dialog.up",
    "dialog.down",
    "dialog.page-up",
    "dialog.page-down",
    "dialog.top",
    "dialog.bottom",
    "dialog.section-prev",
    "dialog.section-next",
    // El selector de columnas y el gestor de extensiones.
    "dialog.toggle-enabled",
    "dialog.move-up",
    "dialog.move-down",
    "dialog.sort",
    "dialog.cycle-format",
    "dialog.add",
    // `dialog.remove` estuvo APLAZADO (#287) con un motivo que era cierto:
    // quitar una fila solo significa algo sobre una lista que se pueda
    // EDITAR, y la única de esta ventana —los ajustes— es de solo lectura.
    // Desde #309 hay una que sí: los favoritos. Y hasta que este nombre entró
    // aquí, atarlo en el preset no hacía nada — el keymap efectivo lo filtra
    // por esta lista, así que la tecla existía y no llegaba a ningún sitio.
    "dialog.remove",
    // Cambiar de lado (comparar, ayuda), volver (ayuda) y filtrar (ayuda).
    "dialog.pane",
    "dialog.back",
    "dialog.filter",
    // Las listas de historia (spec 2026-09-15 D2): abrir en el otro hueco, y
    // vaciar la historia o los populares.
    "dialog.confirm-other",
    "dialog.clear",
];

/// Los comandos del VISOR que este host implementa.
///
/// Lista aparte porque el visor es otra PANTALLA: con él abierto las teclas
/// son suyas, y mezclarlas con las del listado sería un contexto de entrada
/// que no existe en ningún preset.
pub const IMPLEMENTADOS_VISOR: &[&str] = &[
    "viewer.close",
    "viewer.up",
    "viewer.down",
    "viewer.page-up",
    "viewer.page-down",
    "viewer.top",
    "viewer.bottom",
    "viewer.left",
    "viewer.right",
    "viewer.hex",
    "viewer.encoding",
    "viewer.encoding-auto",
    "viewer.zoom-in",
    "viewer.zoom-out",
    "viewer.zoom-fit",
    "viewer.next",
    "viewer.prev",
];

/// Todo lo que el host implementa, en las dos pantallas.
///
/// Es lo que se le pasa a `Effective::build_for` en AMBAS: el keymap efectivo
/// necesita saber qué existe para poder distinguir «este frontend no lo hace»
/// de «norte no lo tiene», y esa pregunta no es por pantalla.
#[must_use]
pub fn todos() -> Vec<&'static str> {
    todos_con(Efectos::Completo)
}

/// Igual, con el modo de efectos dicho.
#[must_use]
pub fn todos_con(efectos: Efectos) -> Vec<&'static str> {
    let mut v = implementados(efectos);
    v.extend_from_slice(IMPLEMENTADOS_VISOR);
    v
}

/// Lo que un comando del VISOR le pide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EfectoVisor {
    /// Cierra el visor.
    Cerrar,
    /// Desplaza tantas líneas (negativo hacia arriba).
    Linea(i64),
    /// Desplaza tantas PÁGINAS (negativo hacia arriba).
    Pagina(i64),
    /// Desplaza tantas COLUMNAS (negativo hacia la izquierda).
    ///
    /// El visor no envuelve: sin esto, la cola de una línea más ancha que la
    /// ventana no estaba en ninguna parte.
    Columna(i64),
    /// Al principio o al final.
    Extremo {
        /// `true` = al final.
        al_final: bool,
    },
    /// Alterna el hexadecimal.
    Hex,
    /// Recarga con el siguiente encoding del ciclo.
    Encoding,
    /// Vuelve a la detección automática.
    EncodingAuto,
    /// Mueve el zoom de la imagen un peldaño (spec 2026-09-20).
    Zoom {
        /// `true` = acercar.
        acercar: bool,
    },
    /// Devuelve la imagen a AJUSTADA.
    ZoomAjustar,
    /// Abre la hermana siguiente (o anterior) de la misma clase, sin salir.
    Hermana {
        /// `true` = la siguiente.
        adelante: bool,
    },
}

/// Traduce un comando de la pantalla del visor a su efecto.
///
/// `None` = el host no lo implementa; quien llama lo convierte en un
/// `Unavailable` que el usuario ve.
#[must_use]
pub fn efecto_visor_de(command: &str, veces: u32) -> Option<EfectoVisor> {
    let n = i64::from(veces.max(1).min(u32::from(u16::MAX)));
    Some(match command {
        "viewer.close" => EfectoVisor::Cerrar,
        "viewer.up" => EfectoVisor::Linea(-n),
        "viewer.down" => EfectoVisor::Linea(n),
        "viewer.page-up" => EfectoVisor::Pagina(-n),
        "viewer.page-down" => EfectoVisor::Pagina(n),
        "viewer.top" => EfectoVisor::Extremo { al_final: false },
        "viewer.bottom" => EfectoVisor::Extremo { al_final: true },
        "viewer.left" => EfectoVisor::Columna(-n),
        "viewer.right" => EfectoVisor::Columna(n),
        "viewer.hex" => EfectoVisor::Hex,
        "viewer.encoding" => EfectoVisor::Encoding,
        "viewer.encoding-auto" => EfectoVisor::EncodingAuto,
        "viewer.zoom-in" => EfectoVisor::Zoom { acercar: true },
        "viewer.zoom-out" => EfectoVisor::Zoom { acercar: false },
        "viewer.zoom-fit" => EfectoVisor::ZoomAjustar,
        "viewer.next" => EfectoVisor::Hermana { adelante: true },
        "viewer.prev" => EfectoVisor::Hermana { adelante: false },
        _ => return None,
    })
}

/// Lo que un comando le pide al hueco con el foco.
///
/// Es el vocabulario INTERNO del host: el renderer nunca lo ve. Existe para
/// que el resolver y el ratón acaben en el mismo sitio — un gesto y una
/// tecla que significan lo mismo tienen que hacer lo mismo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Efecto {
    /// Mueve el cursor tantas filas (negativo hacia arriba).
    Cursor(i64),
    /// Mueve el cursor tantas PÁGINAS (negativo hacia arriba). Cuántas filas
    /// son lo decide el hueco con la ventana que el renderer le dijo.
    Pagina(i64),
    /// Cursor al principio o al final del listado.
    Extremo {
        /// `true` = al final.
        al_final: bool,
    },
    /// Entra en lo que haya bajo el cursor.
    Entrar,
    /// Sube al directorio padre.
    Subir,
    /// Rastro de navegación.
    Rastro {
        /// `true` = atrás.
        atras: bool,
    },
    /// Marca o desmarca la fila del cursor.
    Marcar,
    /// Marca o desmarca la fila del cursor y SUBE (`shift+↑`).
    MarcarSubiendo,
    /// Marca (o desmarca) el tramo de una página y se mueve allí.
    MarcarPagina {
        /// `true` = hacia abajo.
        abajo: bool,
    },
    /// Marca del cursor a un extremo y DESMARCA el otro lado
    /// (`shift+Inicio`/`shift+Fin` de Krusader).
    MarcarHastaElBorde {
        /// `true` = hacia arriba.
        arriba: bool,
    },
    /// Quita todas las marcas.
    DesmarcarTodo,
    /// Mueve el foco al siguiente hueco enfocable (o al anterior).
    ///
    /// Con dos paneles es el cambio de siempre; con más, sigue el ORDEN de
    /// tabulación que resuelve la capa compartida, que ya se salta lo que no
    /// se ve y lo que no se enfoca.
    Foco {
        /// `true` = hacia atrás.
        atras: bool,
        /// `true` = solo paran los LISTADOS; los paneles laterales se saltan.
        ///
        /// Es la diferencia entre `pane.switch` y `layout.focus-next`: el
        /// primero es «el otro panel» de cualquier gestor ortodoxo y el
        /// segundo el recorrido de la pantalla entera. Un solo anillo para
        /// los dos obligaba a dar cinco pulsaciones para volver al listado
        /// de al lado con la barra de sitios, el árbol y el visor abiertos.
        solo_listados: bool,
    },
    /// Designa OTRO hueco como destino de la siguiente operación.
    Destino,
    /// Cambia el tamaño del hueco con el foco. Negativo lo encoge.
    Tamano(i64),
    /// Iguala el peso de los hermanos del hueco con el foco.
    Igualar,
    /// Gira el reparto del hueco con el foco (ADR 0138).
    Girar,
    /// Abre el selector de disposiciones.
    Disposiciones,
    /// Abre el selector de columnas.
    Columnas,
    /// Abre la paleta de comandos.
    Paleta,
    /// Abre «ir a cualquier sitio» (#357).
    IrA,
    /// Abre los ajustes: se leen, se giran y se escriben.
    Ajustes,
    /// Abre el gestor de extensiones, en solo lectura.
    Extensiones,
    /// Las sesiones de agente vistas, y el deshacer de una entera.
    Agentes,
    /// Abre otra PESTAÑA junto al hueco enfocado.
    PestanaNueva,
    /// Cierra la pestaña enfocada. Sin grupo, no hace nada.
    CerrarPestana,
    /// Pasa a la pestaña siguiente —o anterior—, ciclando.
    CiclarPestana {
        /// Hacia atrás.
        atras: bool,
    },
    /// Mueve la pestaña enfocada dentro de su grupo.
    MoverPestana {
        /// Hacia la derecha.
        derecha: bool,
    },
    /// Va a la pestaña `n` (base 1) del grupo enfocado.
    IrAPestana {
        /// Cuál, empezando por 1.
        n: usize,
    },
    /// Parte el hueco enfocado y pone otro LISTADO al lado.
    Partir {
        /// Uno encima de otro en vez de uno al lado del otro.
        vertical: bool,
    },
    /// Cierra el hueco enfocado.
    CerrarHueco,
    /// Abre —o cierra— el hueco auxiliar de este kind.
    AlternarHueco {
        /// `places`, `processes`, `metadata` o `tree`: los que esta ventana sabe
        /// PINTAR. Abrir uno que solo se pintaría en gris no es abrirlo.
        kind: &'static str,
    },
    /// Mueve la fila elegida del TABLERO, sin tener que enfocarlo.
    TaskVecina {
        /// Hacia arriba.
        atras: bool,
    },
    /// Quita del tablero la fila elegida, si ya terminó.
    DescartarTask,
    /// Marca TODAS las filas del panel activo.
    MarcarTodo,
    /// Invierte las marcas del panel activo.
    InvertirMarcas,
    /// Marca —o desmarca— las de la MISMA extensión que la del cursor (#313).
    MarcarExtension {
        /// `true` añade marcas, `false` las quita.
        marcar: bool,
    },
    /// Marca las entradas de una CLASE: carpetas o ficheros (#313).
    MarcarClase {
        /// `true` marca carpetas, `false` ficheros.
        dirs: bool,
    },
    /// Devuelve la selección anterior al último gesto en bloque (#313).
    RestaurarMarcas,
    /// Cambia los PERMISOS POSIX de lo marcado (#314): pide el modo en octal.
    Permisos,
    /// Calcula las sumas de lo marcado, o COMPRUEBA el fichero de sumas bajo
    /// el cursor (#311).
    Sumas {
        /// `true` comprueba contra un fichero de sumas; `false` calcula.
        verificar: bool,
    },
    /// Marca —o desmarca— por PATRÓN: abre el prompt del glob.
    MarcarPatron {
        /// `true` añade marcas, `false` las quita.
        marcar: bool,
    },
    /// Copia al portapapeles las rutas de lo marcado (o de lo señalado).
    CopiarRuta,
    /// Abre lo señalado con la aplicación que el escritorio elija.
    AbrirExterno,
    /// Edita lo señalado con el editor que `[ui] editor` nombre; sin él, lo
    /// mismo que [`Efecto::AbrirExterno`].
    EditarExterno,
    /// Compara DOS ficheros (#312) con el programa de `[ui] diff` —o
    /// `diff -u`—, lanzado por quien hospeda: suelto si abre ventana,
    /// esperándolo y capturando su salida si no.
    CompararFicheros,
    /// Abre un terminal sentado en el directorio del panel activo.
    Terminal,
    /// Entrega la pantalla a la TERMINAL y cierra esta ventana (fase 9).
    ///
    /// No es inerte (ADR 0126) y no escribe un fichero: lo que escribe es la
    /// SESIÓN, y además la suelta y cierra la ventana. Una ventana de solo
    /// mirar no hace ninguna de las tres.
    Relevo,
    /// Enseña el tema activo por dentro.
    Tema,
    /// Despliega la barra de menús. Ni añade capacidades ni las quita:
    /// ofrece las mismas órdenes del catálogo, ordenadas por tema, para
    /// quien no sabe el nombre de lo que busca.
    Menu,
    /// Pide cerrar la ventana: el mismo camino que el botón de cerrar, con
    /// la misma pregunta de `[ui] confirm_quit`.
    Salir,
    /// Abre el selector de PERFILES (ADR 0079).
    PerfilElegir,
    /// Guardar el espacio de trabajo de AHORA como un perfil (#318).
    PerfilGuardarComo,
    /// Salta al perfil siguiente o al anterior, sin abrir nada.
    PerfilVecino {
        /// Hacia el anterior.
        atras: bool,
    },
    /// Abre el selector de volúmenes del host.
    Volumenes,
    /// Abre la ayuda. Sobre la página del CONTEXTO donde está el lector —
    /// un diálogo abierto, el visor, el listado— y no siempre sobre el
    /// índice: quien pulsa F1 mirando una pregunta quiere esa respuesta.
    Ayuda,
    /// Abre el visor sobre la entrada bajo el cursor.
    Ver,
    /// Abre el buscador incremental del listado.
    BuscarRapido,
    /// Pide el PLAN de sincronizar el panel activo sobre el destino.
    ///
    /// El plan NO escribe: dice qué haría. Aun así no es inerte, porque
    /// es la puerta de una escritura y una ventana que se declara de solo
    /// mirar no la abre.
    Sincronizar,
    /// Compara los dos paneles y abre el panel de diferencias.
    ///
    /// NO muta: camina los dos árboles y contesta. Es una tarea larga y
    /// cancelable, y cancelarla es su único freno.
    Comparar,
    /// Empaqueta lo MARCADO en un contenedor nuevo (#132).
    ///
    /// No es inerte (ADR 0126): escribe un fichero. El nombre se teclea, y de él
    /// sale el FORMATO — un nombre sin extensión conocida se rehúsa en vez de
    /// empaquetar en algo que nadie pidió.
    Empaquetar,
    /// Copia el INTERIOR del contenedor bajo el cursor al panel destino
    /// (#132).
    ///
    /// No lleva método propio y no le hace falta: el motor de copia acepta el
    /// interior de un archivo como origen, así que desempaquetar es la copia
    /// que el lector podría haber hecho a mano — con su journal, su undo, su
    /// política de colisiones y su cancelación.
    Desempaquetar,
    /// Comprueba el contenedor bajo el cursor (#132).
    ///
    /// Es inerte (ADR 0126): lee el archivo entero y contesta si está sano,
    /// sin escribir nada. Es la misma categoría que comparar.
    ComprobarArchivo,
    /// El selector de conexiones configuradas (#264).
    ///
    /// Es inerte (ADR 0126): listar no abre nada. Elegir una NAVEGA, y navegar
    /// es lo que establece la sesión — con el mismo gate que cualquier otro
    /// listado, y su TOFU si hace falta.
    Conexiones,
    /// Cierra la sesión del panel activo y lo saca de ahí (#140).
    ///
    /// Es inerte (ADR 0126): soltar una sesión no escribe un byte en ningún
    /// sitio. Lo que sí hace es dejar el panel mirando algo que ya no se puede
    /// leer, y por eso navega a continuación.
    Desconectar,
    /// Parte el fichero bajo el cursor en trozos del tamaño que se teclee
    /// (#132). No es inerte (ADR 0126): escribe los trozos.
    ///
    /// `PartirFichero` y no `Partir` a secas: [`Efecto::Partir`] es partir un
    /// HUECO de la disposición, que no tiene nada que ver.
    PartirFichero,
    /// Junta los trozos a partir del `.001` bajo el cursor (#132). También
    /// escribe, así que tampoco es inerte.
    Juntar,
    /// Cuenta lo que ocupa lo MARCADO —o lo que hay bajo el cursor— (#139).
    ///
    /// Es inerte por lo mismo que [`Efecto::Comparar`]: camina un
    /// árbol y contesta, sin escribir ni sacar nada del proceso que listar no
    /// sacara ya. Es larga y cancelable, y el tablero la enseña como
    /// `dir-size`.
    TamanoDeDirectorio,
    /// Pide una búsqueda SEMÁNTICA contra el índice: abre el prompt de la
    /// consulta.
    ///
    /// No es inerte (ADR 0126) y no escribe un byte: la consulta SALE del proceso
    /// hacia el proveedor de IA configurado, igual que el contenido de un
    /// directorio en [`Efecto::RenameIa`].
    BuscarSemantica,
    /// Abre el prompt de buscar por el subárbol.
    Buscar,
    /// Abre el prompt de crear directorio.
    CrearDirectorio,
    /// Abre el prompt de crear un fichero VACÍO y editarlo (#290).
    ///
    /// No es inerte (ADR 0126): crea un nodo en el disco, con su entrada de journal
    /// y su deshacer, exactamente como crear un directorio.
    CrearFichero,
    /// Pide borrar lo marcado (o lo que haya bajo el cursor). NO borra: abre
    /// la confirmación, que es por donde pasan TODAS las vías —tecla, menú,
    /// gesto—, porque una operación destructiva con dos puertas acaba
    /// teniendo una sin cerrojo.
    Borrar {
        /// Permanente, sin papelera.
        permanente: bool,
    },
    /// Pide copiar o mover lo marcado (o lo que haya bajo el cursor) al hueco
    /// DESTINO. NO transfiere: abre la confirmación, por el mismo motivo que
    /// [`Efecto::Borrar`] —y aquí además la confirmación es lo único que
    /// enseña A DÓNDE va, que en una ventana con tres listados no es
    /// evidente.
    Transferir {
        /// `true` = mover; `false` = copiar. El wire son dos métodos
        /// distintos, así que esto no elige una opción: elige el verbo.
        mover: bool,
    },
    /// Pide un plan de renombrado para el directorio ENTERO. Abre el prompt
    /// de la instrucción; el plan llega después y se revisa antes de nada.
    RenameIa,
    /// Pide un plan de ORGANIZAR para el directorio entero (fase 8). No abre
    /// prompt: lo que se pide es «mira este directorio y propón una forma»,
    /// así que el plan llega solo y se revisa como un árbol antes de nada.
    Organizar,
    /// Renombrar en lote por PLANTILLA (#310): abre el prompt de la
    /// plantilla, y el plan —determinista, sin modelo— entra por la MISMA
    /// revisión que el de la IA.
    RenameLote,
    /// Pide parar una task del tablero.
    ///
    /// Sobrevive a [`Efectos::SoloLectura`] **solo para las tasks propias**, y
    /// la distinción no es formalismo: parar una copia SÍ toca el disco —el
    /// destino se limpia o queda un `.norte-partial`, que es la regla del
    /// proyecto—, así que una ventana montada sin efectos no puede abortar la
    /// transferencia de OTRO cliente y dejarle un parcial. Sus propias tasks
    /// son otra cosa: si pudo lanzarlas, puede pararlas.
    CancelarTask,
    /// Ordena el listado enfocado por esta columna.
    ///
    /// La misma semántica que un click en la cabecera: la columna activa
    /// invierte, una nueva ordena ascendente. Quien lo decide es
    /// `SortSpec::after_click`, no una segunda tabla de aquí.
    ///
    /// La CLAVE de una columna de orden, no la columna: por aquí solo llegan
    /// las teclas de orden (`pane.sort-name`, `-size`…), que son siempre
    /// built-ins. Ordenar por un atributo entra por el clic en su cabecera
    /// (`ordenar_por`), así que meter aquí el `SortColumn` entero —que dejó de
    /// ser `Copy` al poder llevar un id (ADR 0144)— le quitaría `Copy` a todo
    /// `Efecto` por un caso que por este camino no llega nunca.
    Ordenar(norte_config::SortColumnKey),
    /// Vuelve a pedir el listado de los huecos que se ven.
    ///
    /// De TODOS, no solo del enfocado: un cambio externo raramente respeta
    /// el foco, que es por lo que el TUI refresca los dos paneles.
    Refrescar,
    /// Aparta o devuelve los ficheros ocultos del panel activo.
    ///
    /// Presentación-solo (#107): el provider no re-lista.
    AlternarOcultos,
    /// Cicla la reinterpretación de los nombres que no son UTF-8 (#57).
    ///
    /// Display-only, regla 1: los bytes no se tocan.
    CiclarEncoding,
    /// La ubicación del hueco ACTIVO viaja al hueco DESTINO.
    Espejo,
    /// Enciende o apaga la navegación SINCRONIZADA: mientras está puesta,
    /// cada navegación del hueco activo la repite el destino.
    ///
    /// No navega por sí mismo, y por eso no está en el grupo de gestos de
    /// panel de al lado: lo único que hace es mover un interruptor.
    EspejoPermanente,
    /// Como [`Efecto::Espejo`], pero lo que viaja es el OBJETIVO DEL CURSOR:
    /// la carpeta bajo él si lo es, y si no la ubicación del hueco activo
    /// (`Ctrl+←`/`Ctrl+→` de Krusader). Qué directorio es eso lo decide
    /// `PaneState::target_dir`, uno solo para los dos frontends (ADR 0077).
    EspejoObjetivo,
    /// La ubicación del hueco DESTINO viaja al ACTIVO: el espejo al revés.
    Traer,
    /// Los dos huecos —activo y destino— cambian de sitio.
    ///
    /// No toca disco: los dos listados ya existían.
    Intercambiar,
    /// Abre la lista del rastro de navegación del hueco activo.
    Historial,
    /// Abre la lista de favoritos de la configuración.
    Hotlist,
    /// Abre los directorios POPULARES de la sesión (spec 2026-09-15 D6).
    Populares,
    /// Abre la historia de un LADO de la pantalla (D7), resuelto por la
    /// geometría del reparto como en [`Efecto::VolumenesDeLado`].
    HistorialDeLado {
        /// El de más a la derecha en vez del de más a la izquierda.
        derecha: bool,
    },
    /// Vuelve al punto de salto del hueco activo (D5).
    SaltoAtras,
    /// Fija el punto de salto en el directorio del hueco activo (D5).
    FijarSalto,
    /// Abre el selector de volúmenes para un LADO de la pantalla.
    ///
    /// Un lado, no el foco: es lo que hacen `Alt+F1`/`Alt+F2` de Total
    /// Commander, y lo que el TUI hace con sus `panes[0]`/`panes[1]`. Aquí
    /// el lado lo decide la GEOMETRÍA del reparto, que es lo único que en
    /// un árbol de huecos significa «izquierda».
    VolumenesDeLado {
        /// El de más a la derecha en vez del de más a la izquierda.
        derecha: bool,
    },
    /// Pide renombrar la entrada bajo el cursor. NO renombra: abre el nombre
    /// para editarlo.
    ///
    /// Por el WIRE es un movimiento al mismo directorio, y aun así es un
    /// efecto propio: lo que pregunta es otra cosa (un nombre, no un sitio),
    /// lo que rehúsa es otra cosa (una selección múltiple, no un destino que
    /// falta) y lo que siembra el campo tiene una regla que ninguna otra
    /// superficie tiene — el nombre SIN TOCAR viaja como bytes.
    Renombrar,
}

/// Traduce un comando del catálogo al efecto que el host aplica.
///
/// `None` = el host no lo implementa. No es un descarte silencioso: quien
/// llama lo convierte en un `Unavailable` que el usuario ve.
#[must_use]
// Una TABLA: un brazo por comando del catálogo, y cada brazo es un nombre.
// Larga por número de comandos, no por lógica — partirla en dos mitades
// arbitrarias solo escondería la mitad, y lo que hace legible una tabla es
// verla entera. Mismo criterio que el reparto de mensajes del actor.
#[expect(
    clippy::too_many_lines,
    reason = "tabla comando→efecto: legible entera, como el reparto de mensajes del actor"
)]
pub fn efecto_de(command: &str, veces: u32) -> Option<Efecto> {
    let n = i64::from(veces.max(1).min(u32::from(u16::MAX)));
    Some(match command {
        "cursor.up" => Efecto::Cursor(-n),
        "cursor.down" => Efecto::Cursor(n),
        // Una página son las filas VISIBLES, y cuántas son lo sabe el hueco
        // (el renderer se lo dijo con `SetVisibleRange`): por eso viaja como
        // páginas y no como filas.
        "cursor.page-up" => Efecto::Pagina(-n),
        "cursor.page-down" => Efecto::Pagina(n),
        "cursor.top" => Efecto::Extremo { al_final: false },
        "cursor.bottom" => Efecto::Extremo { al_final: true },
        "nav.enter" => Efecto::Entrar,
        "nav.parent" => Efecto::Subir,
        "nav.back" => Efecto::Rastro { atras: true },
        "nav.forward" => Efecto::Rastro { atras: false },
        "nav.jump-back" => Efecto::SaltoAtras,
        "nav.set-jump-point" => Efecto::FijarSalto,
        "mark.toggle" => Efecto::Marcar,
        "mark.clear" => Efecto::DesmarcarTodo,
        "mark.all" => Efecto::MarcarTodo,
        "mark.invert" => Efecto::InvertirMarcas,
        "mark.pattern-add" => Efecto::MarcarPatron { marcar: true },
        "mark.pattern-remove" => Efecto::MarcarPatron { marcar: false },
        "mark.extension-add" => Efecto::MarcarExtension { marcar: true },
        "mark.extension-remove" => Efecto::MarcarExtension { marcar: false },
        "mark.files" => Efecto::MarcarClase { dirs: false },
        "mark.dirs" => Efecto::MarcarClase { dirs: true },
        "mark.restore" => Efecto::RestaurarMarcas,
        "mark.toggle-up" => Efecto::MarcarSubiendo,
        "mark.toggle-page-down" => Efecto::MarcarPagina { abajo: true },
        "mark.toggle-page-up" => Efecto::MarcarPagina { abajo: false },
        "mark.to-top" => Efecto::MarcarHastaElBorde { arriba: true },
        "mark.to-bottom" => Efecto::MarcarHastaElBorde { arriba: false },
        // `pane.switch` es «el otro panel»: cicla los LISTADOS, todos los que
        // haya, y ninguno más. `layout.focus-*` es el recorrido de la pantalla
        // entera, laterales incluidos. Compartían brazo, y eso hacía que con
        // el árbol y el visor abiertos el tabulador diera cinco paradas para
        // volver al listado de al lado.
        "pane.switch" => Efecto::Foco {
            atras: false,
            solo_listados: true,
        },
        "layout.focus-next" => Efecto::Foco {
            atras: false,
            solo_listados: false,
        },
        "layout.focus-prev" => Efecto::Foco {
            atras: true,
            solo_listados: false,
        },
        "layout.set-target" => Efecto::Destino,
        "layout.grow" => Efecto::Tamano(n),
        "layout.shrink" => Efecto::Tamano(-n),
        "layout.equalize" => Efecto::Igualar,
        "layout.flip" => Efecto::Girar,
        "layout.pick" => Efecto::Disposiciones,
        "layout.split-h" => Efecto::Partir { vertical: false },
        "layout.split-v" => Efecto::Partir { vertical: true },
        "layout.close-slot" => Efecto::CerrarHueco,
        "layout.places" => Efecto::AlternarHueco { kind: "places" },
        "layout.processes" => Efecto::AlternarHueco { kind: "processes" },
        "layout.log" => Efecto::AlternarHueco { kind: "log" },
        "layout.disk-map" => Efecto::AlternarHueco { kind: "disk-map" },
        "layout.timeline" => Efecto::AlternarHueco { kind: "timeline" },
        // El último de los siete de la ADR 0058 (#291): el visor acoplado.
        "layout.preview" => Efecto::AlternarHueco { kind: "viewer" },
        "pane.tree" => Efecto::AlternarHueco { kind: "tree" },
        // `pane.properties` cae aquí a propósito: las propiedades de esta
        // ventana SON la hoja de atributos, que ya enseña nombre, clase,
        // tamaño y fecha de lo señalado. Lo hace de otra forma, igual que
        // ordena pulsando la cabecera.
        "layout.metadata" | "pane.properties" => Efecto::AlternarHueco { kind: "metadata" },
        "pane.tab-new" => Efecto::PestanaNueva,
        "pane.tab-close" => Efecto::CerrarPestana,
        "pane.tab-next" => Efecto::CiclarPestana { atras: false },
        "pane.tab-prev" => Efecto::CiclarPestana { atras: true },
        "pane.tab-move-left" => Efecto::MoverPestana { derecha: false },
        "pane.tab-move-right" => Efecto::MoverPestana { derecha: true },
        "pane.tab-goto-1" => Efecto::IrAPestana { n: 1 },
        "pane.tab-goto-2" => Efecto::IrAPestana { n: 2 },
        "pane.tab-goto-3" => Efecto::IrAPestana { n: 3 },
        "pane.tab-goto-4" => Efecto::IrAPestana { n: 4 },
        "pane.tab-goto-5" => Efecto::IrAPestana { n: 5 },
        "pane.tab-goto-6" => Efecto::IrAPestana { n: 6 },
        "pane.tab-goto-7" => Efecto::IrAPestana { n: 7 },
        "pane.tab-goto-8" => Efecto::IrAPestana { n: 8 },
        "pane.tab-goto-9" => Efecto::IrAPestana { n: 9 },
        // El «menú de orden» ES el diálogo de columnas: ahí están la columna,
        // la dirección y `dirs_first`. Una segunda pantalla para lo mismo
        // sería otra que mantener y otra que aprender, y es la misma decisión
        // que tomó el TUI.
        "pane.columns" | "pane.sort-menu" => Efecto::Columnas,
        "app.palette" => Efecto::Paleta,
        "app.goto" => Efecto::IrA,
        "app.help" => Efecto::Ayuda,
        "app.settings" => Efecto::Ajustes,
        "app.quit" => Efecto::Salir,
        "app.extensions" => Efecto::Extensiones,
        "app.agents" => Efecto::Agentes,
        "pane.copy-path" => Efecto::CopiarRuta,
        // F4 lanza el editor que `[ui] editor` nombre, y si no hay ninguno
        // cae en `pane.open` — o sea en `openers.toml` y, en último término,
        // en la aplicación del ESCRITORIO.
        //
        // Lo que sigue fuera es `$EDITOR` (#290), y sigue siendo deliberado:
        // el TUI lo lanza porque ya está dentro de un terminal, y esta
        // ventana no tiene uno donde ponerlo. `[ui] editor` es otra cosa —un
        // programa que el lector nombra, y que puede ser gráfico— y su clave
        // hermana `[ui] diff` ya la honra esta ventana.
        "pane.open" => Efecto::AbrirExterno,
        "pane.edit" => Efecto::EditarExterno,
        "pane.compare-files" => Efecto::CompararFicheros,
        "app.terminal" => Efecto::Terminal,
        "app.handoff" => Efecto::Relevo,
        "app.theme" => Efecto::Tema,
        "app.menu" => Efecto::Menu,
        "profile.pick" => Efecto::PerfilElegir,
        "profile.save-as" => Efecto::PerfilGuardarComo,
        "profile.next" => Efecto::PerfilVecino { atras: false },
        "profile.prev" => Efecto::PerfilVecino { atras: true },
        "pane.select-drive" => Efecto::Volumenes,
        "pane.connect" => Efecto::Conexiones,
        "pane.disconnect" => Efecto::Desconectar,
        "pane.view" => Efecto::Ver,
        "pane.quick-search" => Efecto::BuscarRapido,
        "pane.search" => Efecto::Buscar,
        "pane.mkdir" => Efecto::CrearDirectorio,
        "pane.edit-new" => Efecto::CrearFichero,
        "pane.delete" => Efecto::Borrar { permanente: false },
        "pane.delete-permanent" => Efecto::Borrar { permanente: true },
        "pane.copy" => Efecto::Transferir { mover: false },
        "pane.move" => Efecto::Transferir { mover: true },
        "pane.rename" => Efecto::Renombrar,
        "pane.chmod" => Efecto::Permisos,
        "pane.checksum" => Efecto::Sumas { verificar: false },
        "pane.checksum-verify" => Efecto::Sumas { verificar: true },
        "pane.ai-rename" => Efecto::RenameIa,
        "pane.organize" => Efecto::Organizar,
        "pane.rename-batch" => Efecto::RenameLote,
        "pane.semantic-search" => Efecto::BuscarSemantica,
        "pane.compare-dirs" => Efecto::Comparar,
        "pane.dir-size" => Efecto::TamanoDeDirectorio,
        "pane.pack" => Efecto::Empaquetar,
        "pane.unpack" => Efecto::Desempaquetar,
        "pane.test-archive" => Efecto::ComprobarArchivo,
        "pane.split-file" => Efecto::PartirFichero,
        "pane.combine-files" => Efecto::Juntar,
        "pane.sync-dirs" => Efecto::Sincronizar,
        // #138: la misma semántica que un click en la cabecera, y sobre el
        // hueco con el FOCO — el orden es de un listado, como el cursor.
        "pane.sort-name" => Efecto::Ordenar(norte_config::SortColumnKey::Name),
        "pane.sort-ext" => Efecto::Ordenar(norte_config::SortColumnKey::Extension),
        "pane.sort-size" => Efecto::Ordenar(norte_config::SortColumnKey::Size),
        "pane.sort-time" => Efecto::Ordenar(norte_config::SortColumnKey::Mtime),
        "pane.refresh" => Efecto::Refrescar,
        "pane.toggle-hidden" => Efecto::AlternarOcultos,
        "pane.names-encoding" => Efecto::CiclarEncoding,
        "pane.mirror" => Efecto::Espejo,
        "pane.sync-nav" => Efecto::EspejoPermanente,
        "pane.mirror-target" => Efecto::EspejoObjetivo,
        "pane.pull" => Efecto::Traer,
        "pane.swap" => Efecto::Intercambiar,
        "pane.history" => Efecto::Historial,
        "pane.hotlist" => Efecto::Hotlist,
        "pane.popular" => Efecto::Populares,
        "pane.history-left" => Efecto::HistorialDeLado { derecha: false },
        "pane.history-right" => Efecto::HistorialDeLado { derecha: true },
        "pane.select-drive-left" => Efecto::VolumenesDeLado { derecha: false },
        "pane.select-drive-right" => Efecto::VolumenesDeLado { derecha: true },
        otro => return efecto_del_tablero(otro),
    })
}

/// La cola de [`efecto_de`]: lo que actúa sobre el TABLERO de tasks.
///
/// Vive aparte porque el `match` de una sola función se pasa del tope de
/// líneas, y este es el corte natural: todo lo de arriba actúa sobre un
/// listado o sobre lo que se enseña encima de él; esto, sobre las tareas en
/// marcha, que no son ni una cosa ni la otra.
fn efecto_del_tablero(command: &str) -> Option<Efecto> {
    Some(match command {
        "task.next" => Efecto::TaskVecina { atras: false },
        "task.prev" => Efecto::TaskVecina { atras: true },
        "task.dismiss" => Efecto::DescartarTask,
        "task.cancel" => Efecto::CancelarTask,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Todo lo que se declara implementado tiene efecto, y al revés. Sin
    /// esto, la lista y el `match` se separan y `Availability` empieza a
    /// mentir.
    #[test]
    fn la_lista_y_los_efectos_no_pueden_separarse() {
        for c in IMPLEMENTADOS {
            assert!(
                efecto_de(c, 1).is_some(),
                "{c} está en la lista y no tiene efecto"
            );
        }
    }

    /// Lo mismo para la pantalla del visor.
    #[test]
    fn la_lista_del_visor_y_sus_efectos_no_pueden_separarse() {
        for c in IMPLEMENTADOS_VISOR {
            assert!(
                efecto_visor_de(c, 1).is_some(),
                "{c} está en la lista del visor y no tiene efecto"
            );
        }
    }

    /// Y las dos listas son disjuntas: un comando en las dos significaría que
    /// una tecla hace dos cosas distintas según la pantalla sin que nadie lo
    /// declare.
    #[test]
    fn las_dos_pantallas_no_comparten_comandos() {
        for c in IMPLEMENTADOS_VISOR {
            assert!(
                !IMPLEMENTADOS.contains(c),
                "{c} está declarado en las dos pantallas"
            );
        }
    }

    /// Y todo lo declarado existe en el catálogo compartido: un comando
    /// inventado aquí no lo ligaría ningún preset.
    #[test]
    fn todo_lo_declarado_esta_en_el_catalogo() {
        for c in todos() {
            assert!(
                norte_frontend::keymap::CATALOGUE
                    .iter()
                    .any(|d| d.name == c),
                "{c} no está en el catálogo compartido"
            );
        }
    }

    /// Solo lectura quita EXACTAMENTE lo que el catálogo no llama inerte, y
    /// son los veinticuatro que la lista `MUTAN` enumeraba a mano antes de
    /// ADR 0126: derivarlos no podía cambiar qué hace la ventana.
    #[test]
    fn solo_lectura_quita_lo_que_no_es_inerte() {
        let solo_lectura = implementados(Efectos::SoloLectura);
        let mut quitados: Vec<&str> = IMPLEMENTADOS
            .iter()
            .copied()
            .filter(|c| !solo_lectura.contains(c))
            .collect();
        quitados.sort_unstable();
        assert_eq!(
            quitados,
            [
                "app.handoff",
                "app.terminal",
                "pane.ai-rename",
                "pane.checksum",
                "pane.checksum-verify",
                "pane.chmod",
                "pane.combine-files",
                "pane.compare-files",
                "pane.copy",
                "pane.delete",
                "pane.delete-permanent",
                "pane.edit",
                "pane.edit-new",
                "pane.mkdir",
                "pane.move",
                "pane.open",
                "pane.organize",
                "pane.pack",
                "pane.rename",
                "pane.rename-batch",
                "pane.semantic-search",
                "pane.split-file",
                "pane.sync-dirs",
                "pane.unpack",
            ]
        );
        for c in &solo_lectura {
            assert!(inerte(c), "{c} sobrevive a solo lectura sin ser inerte");
        }
    }

    /// El visor y los diálogos no se filtran en solo lectura: sus listas se
    /// sirven enteras. Eso sólo es correcto mientras TODO lo que tienen sea
    /// inerte, y este test lo exige — un `viewer.edit` que lanzara un editor
    /// se colaría, si no, en la ventana que prometió sólo mirar (ADR 0126).
    #[test]
    fn el_visor_y_los_dialogos_solo_tienen_comandos_inertes() {
        for c in IMPLEMENTADOS_VISOR.iter().chain(IMPLEMENTADOS_DIALOGO) {
            assert!(
                inerte(c),
                "{c} no es inerte y su lista no se filtra en solo lectura"
            );
        }
    }

    /// Un nombre que el catálogo no conoce no es inerte.
    #[test]
    fn lo_desconocido_no_es_inerte() {
        assert!(!inerte("pane.no-existe-jamas"));
        assert!(inerte("cursor.down"));
    }

    /// El contador multiplica lo que se puede repetir.
    #[test]
    fn el_contador_multiplica() {
        assert_eq!(efecto_de("cursor.down", 3), Some(Efecto::Cursor(3)));
        assert_eq!(efecto_de("cursor.up", 3), Some(Efecto::Cursor(-3)));
        assert_eq!(efecto_de("cursor.page-down", 2), Some(Efecto::Pagina(2)));
    }
}
