//! Lo que el renderer VE. Nada más, y sobre todo, nada con autoridad.
//!
//! Tres reglas gobiernan este módulo, y las tres existen por el mismo motivo
//! —que el renderer no puede ser autoridad de nada (ADR 0066)—:
//!
//! 1. **Ningún path crudo cruza.** Ni `VPath`, ni `PathBuf`, ni `OsString`.
//!    Lo que viaja es el texto YA saneado y una marca de si difiere del
//!    nombre real. Para actuar sobre una fila se usa su [`RowKey`] opaca.
//! 2. **Todo lo pintable está acotado en Rust** ([`crate::bridge`]).
//! 3. **Nada aquí decide.** Un `enabled: false` es lo que el host resolvió;
//!    el renderer lo pinta, no lo calcula.

use serde::{Deserialize, Serialize};

use crate::bridge::{ModalId, RowKey};

/// El estado COMPLETO de la pantalla.
///
/// Un `Snapshot` reemplaza lo que el renderer tuviera: es la única forma de
/// recuperarse de un hueco en la secuencia, y por eso se manda entero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewSnapshot {
    /// Estado de la conexión con el daemon.
    pub connection: ConnectionView,
    /// Dónde va cada hueco y con qué papel. El renderer NO reparte la
    /// pantalla: la recibe repartida (ADR 0066, decisión D14).
    pub layout: LayoutView,
    /// Los huecos de la disposición, por id.
    pub slots: Vec<SlotView>,
    /// Hueco con el foco de teclado.
    pub focus: Option<u32>,
    /// La barra de estado.
    pub status: StatusView,
    /// Diálogos abiertos, en orden de apertura.
    pub dialogs: Vec<DialogView>,
    /// Tasks vivas y las que acaban de terminar.
    pub tasks: Vec<TaskView>,
    /// La paleta de comandos, si está abierta.
    pub palette: Option<PaletteView>,
    /// El panel de continuaciones, si hay un prefijo a medias.
    pub whichkey: Option<WhichKeyView>,
    /// La ayuda, si está abierta. Como el visor, ocupa la pantalla: mientras
    /// esté, las teclas son suyas.
    pub help: Option<HelpView>,
    /// Las extensiones, si están abiertas. Solo LECTURA: se ve qué hay
    /// instalado y en qué estado, y NO se aprueba ni se enciende nada.
    pub extensions: Option<ExtensionsView>,
    /// Los ajustes, si están abiertos. Solo LECTURA: esta ventana enseña lo
    /// que hay y no escribe nada hasta que la fase 5 dé el camino seguro.
    pub settings: Option<SettingsView>,
    /// El visor, si hay uno abierto. Ocupa la pantalla: mientras esté, las
    /// teclas son suyas y el listado no se mueve por debajo.
    pub viewer: Option<ViewerView>,
    /// Idioma negociado, para que el renderer pida el catálogo correcto.
    pub locale: String,
}

/// La paleta de comandos abierta.
///
/// El filtrado, el cursor y qué está seleccionado los decide
/// `norte_frontend::palette_state`, el mismo modelo que el TUI: teclear para
/// acotar una lista es una regla de presentación, y dos copias son dos
/// paletas que se comportan distinto sin que nadie lo note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaletteView {
    /// Lo tecleado, ya saneado para pintar.
    pub query: String,
    /// Las filas que CASAN, en orden.
    pub rows: Vec<PaletteRowView>,
    /// Cuál está seleccionada, si hay alguna.
    pub cursor: Option<u64>,
    /// Cuántas filas hay en total, para decir cuánto se está acotando.
    pub total: u64,
}

/// Un comando ofrecido por la paleta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaletteRowView {
    /// Lo que se enseña (el nombre del comando, o el título ya enmascarado
    /// de un comando de plugin). NUNCA la clave de despacho.
    pub text: String,
    /// Qué hace, en el idioma del usuario.
    pub desc: String,
    /// El atajo que lo corre, o `—` si no tiene ninguno en este preset.
    pub chord: String,
    /// Este frontend puede ejecutarlo.
    pub enabled: bool,
}

/// Lo que puede seguir a un prefijo a medias.
///
/// Se construye con `norte_frontend::whichkey`, que es el mismo modelo que
/// pinta el TUI: qué teclas continúan la secuencia, cómo se llama cada una en
/// el idioma del usuario, cuáles abren otra secuencia y cuáles no se pueden
/// hacer aquí. El renderer lo pinta; no sabe resolver un prefijo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhichKeyView {
    /// El prefijo tecleado, ya pintado, con el contador delante si lo hay.
    pub title: String,
    /// Una fila por tecla que puede seguir, en el orden compartido.
    pub rows: Vec<WhichKeyRowView>,
}

/// Una continuación posible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhichKeyRowView {
    /// La tecla, escrita para leerse (`F5`) y enmascarada: un `keymap.toml`
    /// de proyecto puede ligar cualquier punto de código.
    pub chord: String,
    /// Qué hace, en el idioma del usuario.
    pub label: String,
    /// Se puede hacer aquí.
    pub enabled: bool,
    /// Abre OTRA secuencia en vez de ejecutar algo. El renderer lo marca en
    /// vez de nombrar un comando que la tecla no ejecuta.
    pub opens_sequence: bool,
    /// Por qué no se puede, ya traducido. Vacío cuando sí se puede.
    pub reason: String,
}

/// La ayuda abierta (F1).
///
/// El corpus, el modelo del overlay y la resolución de las marcas vivas son
/// los COMPARTIDOS (`norte_help`, `norte_frontend::help`,
/// `norte_frontend::help_chords`): qué páginas hay, cuál está abierta, qué
/// filas se pueden correr y con qué tecla las corre ESTE lector. El renderer
/// no interpreta markdown y no resuelve una tecla: recibe bloques cerrados y
/// los pinta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpView {
    /// El título de la página abierta, ya acotado.
    pub title: String,
    /// Su id: una IDENTIDAD OPACA, no algo que se pinte.
    ///
    /// Viaja ENTERA o no viaja. No pasa por el recorte de pantalla, que es lo
    /// que hace el resto de este módulo, porque recortar no es inyectivo y
    /// esto es una clave: dos ids que coincidieran en sus primeros miles de
    /// bytes llegarían como una sola (ADR 0061, y el mismo motivo por el que
    /// `norte_help::parse_untrusted` copia el id verbatim). Un id que no
    /// cupiera viaja VACÍO, que es una identidad que no casa con nada, en vez
    /// de una que casa con la equivocada.
    ///
    /// Tampoco está enmascarada, y por eso **el renderer no la pinta jamás**:
    /// quien quiera marcar la fila viva de la lateral tiene
    /// [`HelpSidebarRowView::Topic::current`], que ya viene resuelto.
    pub topic_id: String,
    /// La línea de procedencia de una página de plugin (quién la publica, si
    /// se recortó, si hubo bytes que no decodificaron). `None` en una página
    /// del binario: una página de plugin SIEMPRE lleva línea, y una que a
    /// veces aparece enseña lo contrario de la verdad cuando falta.
    pub badge: Option<String>,
    /// La lateral: cabeceras de grupo y páginas, en el orden del modelo.
    pub sidebar: Vec<HelpSidebarRowView>,
    /// Qué fila de la lateral tiene el cursor.
    pub cursor: u64,
    /// Qué mitad tiene el teclado.
    pub focus: HelpFocusView,
    /// El cuerpo de la página, en bloques de un vocabulario CERRADO.
    pub blocks: Vec<HelpBlockView>,
    /// Lo que `enter` puede hacer sobre el cuerpo: correr un comando o abrir
    /// otra página.
    pub actions: Vec<HelpActionView>,
    /// Cuál está elegida, si hay alguna.
    pub action_cursor: Option<u64>,
    /// Lo tecleado en el filtro, ya saneado para pintar.
    pub filter: String,
    /// El filtro está abierto: las teclas de texto son suyas.
    pub filtering: bool,
    /// Hay a dónde volver (`⌫`). Cuando no lo hay, `⌫` cierra.
    pub can_back: bool,
}

/// Qué mitad del overlay tiene el cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelpFocusView {
    /// La lateral: arriba y abajo cambian de página.
    Topics,
    /// El cuerpo: arriba y abajo recorren lo ejecutable, `enter` actúa.
    Body,
}

/// Una fila de la lateral.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "row")]
pub enum HelpSidebarRowView {
    /// Cabecera de grupo, YA traducida. No se puede elegir.
    Group {
        /// El texto de la cabecera.
        label: String,
    },
    /// Una página que el lector puede abrir.
    Topic {
        /// Su título, ya acotado.
        title: String,
        /// Es la que está abierta.
        current: bool,
    },
}

/// Un bloque del cuerpo. Vocabulario CERRADO (ADR 0040): que un `help.md`
/// hostil no pueda expresar nada fuera de esta lista es precisamente lo que
/// lo hace seguro, y el renderer construye nodos del DOM uno a uno — nunca
/// HTML — porque un bloque no es marcado, es datos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "block")]
pub enum HelpBlockView {
    /// Encabezado de nivel 1..=3.
    Heading {
        /// El nivel, ya acotado a 1..=3.
        level: u8,
        /// Su texto.
        text: String,
    },
    /// Un párrafo.
    Paragraph {
        /// Sus fragmentos.
        spans: Vec<HelpSpanView>,
    },
    /// Una lista de puntos, de un solo nivel.
    Bullets {
        /// Cada punto, con sus fragmentos.
        items: Vec<Vec<HelpSpanView>>,
    },
    /// Un bloque de código, literal.
    Code {
        /// El lenguaje que declaraba la valla, si lo declaraba.
        lang: Option<String>,
        /// El contenido, sin marcas interpretadas.
        text: String,
    },
    /// Una tabla simple. Las filas llegan YA normalizadas al ancho de la
    /// cabecera, así que el renderer indexa por columna sin comprobar nada.
    Table {
        /// La cabecera.
        header: Vec<String>,
        /// Las filas.
        rows: Vec<Vec<String>>,
    },
    /// Un aviso destacado.
    Callout {
        /// De qué clase.
        kind: HelpCalloutView,
        /// Su contenido.
        spans: Vec<HelpSpanView>,
    },
    /// La hoja de referencia de teclado: cada tecla ligada de una pantalla,
    /// en el orden de precedencia REAL del mapa efectivo.
    ///
    /// Un bloque propio y no una tabla, porque no es prosa del corpus: se
    /// genera del keymap del lector, así que un rebind la cambia, y sus
    /// filas llevan disponibilidad y motivo que una celda de tabla no tiene
    /// dónde poner.
    Keys {
        /// Las filas, en orden.
        rows: Vec<HelpKeyRowView>,
    },
}

/// De qué clase es un aviso destacado.
///
/// Un enum y no una cadena: el renderer compone una clave Fluent con esto
/// (`help-callout-{kind}`) y `t` contesta una clave que no tiene con la clave
/// misma, así que una clase inesperada pintaría `help-callout-…` al lector —
/// el mismo eco que el resto de este módulo se cuida de no producir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelpCalloutView {
    /// Nota neutra.
    Note,
    /// Aviso: algo puede salir mal.
    Warn,
    /// Truco: algo va más rápido.
    Tip,
}

/// Un fragmento dentro de un bloque.
///
/// Las dos marcas VIVAS del corpus (`{{cmd:id}}` y `[[topic]]`) llegan aquí ya
/// resueltas contra el keymap y el idioma de ESTE lector: la prosa no puede
/// mentir sobre una tecla porque nunca lleva una escrita.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "span")]
pub enum HelpSpanView {
    /// Texto llano.
    Text {
        /// El texto.
        text: String,
    },
    /// Énfasis fuerte.
    Strong {
        /// El texto.
        text: String,
    },
    /// Énfasis.
    Emph {
        /// El texto.
        text: String,
    },
    /// Código en línea.
    Code {
        /// El texto.
        text: String,
    },
    /// Un comando, ya resuelto: la tecla que lo corre para este lector, o su
    /// nombre cuando no tiene ninguna (nunca una tecla inventada).
    Command {
        /// Lo que se pinta.
        text: String,
        /// Es una TECLA y no un nombre. El renderer la pinta como tal.
        is_chord: bool,
    },
    /// Un enlace a otra página, YA resuelto a su título.
    ///
    /// No lleva el id de destino, y no es un olvido: una marca `[[topic]]` en
    /// la prosa no está en la lista de acciones —esa la forman los comandos
    /// de la página y sus «ver también»—, así que no hay nada que activar con
    /// ella. Mandar la clave a un renderer que no puede usarla solo conseguía
    /// que un id de tercero, que nadie enmascara porque es una clave, acabara
    /// en un atributo del DOM.
    Link {
        /// Su título, o el id si el corpus de este idioma no la tiene.
        text: String,
    },
}

/// Una fila de la hoja de teclado.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpKeyRowView {
    /// La secuencia PINTADA (`F5`, `g g`) y enmascarada: un `keymap.toml` de
    /// proyecto puede ligar cualquier punto de código.
    pub chord: String,
    /// Qué hace, en el idioma del lector.
    pub label: String,
    /// Esta build puede correrlo.
    pub enabled: bool,
    /// Por qué no, ya traducido. Vacío cuando sí.
    pub reason: String,
}

/// Algo que `enter` puede hacer sobre el cuerpo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpActionView {
    /// Cómo se llama, en el idioma del lector.
    pub label: String,
    /// El atajo que lo corre, vacío si no tiene ninguno o si abre una página.
    pub chord: String,
    /// Se puede hacer AHORA, con los hechos congelados al abrir la ayuda.
    pub enabled: bool,
    /// Por qué no, ya traducido. Vacío cuando sí.
    pub reason: String,
    /// Abre otra página en vez de ejecutar un comando.
    pub opens_topic: bool,
}

/// Los ajustes abiertos (solo lectura).
///
/// El registro, el valor efectivo de cada entrada y su texto localizado son
/// los COMPARTIDOS (`norte_frontend::settings`): el mismo catálogo que pinta
/// el TUI, con los mismos ids estables. Lo que este host añade es la
/// proyección y una sección más —dónde vive cada cosa—, que es diagnóstico y
/// no configuración.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsView {
    /// Las secciones, en su orden.
    pub sections: Vec<SettingsSectionView>,
    /// Qué fila tiene el cursor, contando TODAS las filas de todas las
    /// secciones en orden (las cabeceras no cuentan: no se pueden elegir).
    pub cursor: u64,
    /// Esta ventana no escribe ajustes todavía, y lo DICE en vez de ofrecer
    /// un `enter` que se negaría. Lo pinta el renderer como un aviso, no como
    /// un botón apagado que invita a probar.
    pub read_only: bool,
}

/// Una sección de los ajustes: entradas del registro, o ubicaciones.
///
/// Un enum y no un struct con dos listas: una sección es de una clase o de la
/// otra, y un struct con `rows` y `paths` obligaría a cada renderer a decidir
/// qué hacer cuando llegan las dos llenas — una combinación que no existe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "section")]
pub enum SettingsSectionView {
    /// Entradas del registro con su valor efectivo.
    Settings {
        /// Su título, ya traducido.
        title: String,
        /// Sus filas.
        rows: Vec<SettingRowView>,
    },
    /// Dónde vive cada cosa.
    Paths {
        /// Su título, ya traducido.
        title: String,
        /// Sus filas.
        rows: Vec<PathRowView>,
    },
}

/// Una entrada del registro con su valor efectivo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingRowView {
    /// El id estable del catálogo (`ui.confirm-quit`). Una IDENTIDAD, no algo
    /// que se pinte: viaja para que un renderer pueda anclar una fila entre
    /// dos pintados, y por eso no pasa por el recorte de pantalla.
    pub id: String,
    /// Cómo se llama, en el idioma del lector.
    pub name: String,
    /// Qué hace.
    pub desc: String,
    /// Su valor EFECTIVO, ya resuelto sobre las capas de configuración y como
    /// texto para pintar.
    pub value: String,
    /// Cambiarlo pide reiniciar la ventana.
    pub restart_required: bool,
}

/// Dónde vive cada cosa: las capas de configuración, el estado, los logs y el
/// socket del daemon.
///
/// Es una sección de los ajustes y no una vista aparte porque responde a la
/// misma pregunta que el resto —«¿de dónde sale lo que estoy viendo?»— y
/// porque el catálogo no tiene comando para abrirla.
///
/// Lleva RUTAS y por eso lleva la misma marca que un nombre de fichero: el
/// texto ya saneado, y una bandera de si difiere del real. Ningún valor
/// secreto entra aquí: son ubicaciones, no contenidos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathRowView {
    /// Qué es, ya traducido.
    pub label: String,
    /// Dónde, ya saneado para pintar.
    pub display: String,
    /// El texto de arriba DIFIERE de la ruta real.
    pub hostile: bool,
    /// El sitio no existe (una capa que nadie ha creado). Se DICE, en vez de
    /// enseñar una ruta que parece estar ahí.
    pub missing: bool,
}

/// El gestor de extensiones abierto (solo lectura).
///
/// Aprobar una capability es una decisión de SEGURIDAD y es una mutación:
/// esta ventana la enseña y no la toma, igual que no borra. El camino seguro
/// lo da la fase 5.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionsView {
    /// Lo instalado, en el orden que dio el catálogo.
    pub rows: Vec<ExtensionRowView>,
    /// Cuál está elegida.
    pub cursor: u64,
    /// La ficha de la elegida, cuando ya llegó su esquema. `None` mientras
    /// se pide, o si no se pidió.
    pub detail: Option<ExtensionDetailView>,
    /// El catálogo todavía no ha llegado. Se DICE, en vez de enseñar una
    /// lista vacía que se lee como «no tienes ninguna».
    pub loading: bool,
    /// Directorios que el daemon no pudo cargar, ya saneados. Se enseñan:
    /// una extensión que falla al cargar y desaparece en silencio es una
    /// extensión que el usuario cree tener.
    pub errors: Vec<ExtensionErrorView>,
}

/// Una extensión del catálogo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionRowView {
    /// Su id, reverse-DNS validado. Es una IDENTIDAD: viaja entera y sin
    /// recortar, y sirve para pedir su ficha.
    pub id: String,
    /// Su nombre, ya enmascarado y acotado (texto de tercero).
    pub name: String,
    /// Quién la publica, ya enmascarado. Vacío si no lo declara.
    pub publisher: String,
    /// Su versión, ya enmascarada: la declara el manifiesto, o sea un
    /// tercero, y acaba en una fila.
    pub version: String,
    /// Qué papel juega (`previewer`, `indexer`…), del vocabulario del core.
    pub category: String,
    /// Qué hace, ya enmascarada y acotada. Vacía si no lo declara.
    pub description: String,
    /// Un humano aprobó sus capabilities.
    pub approved: bool,
    /// Un humano la tiene encendida.
    pub enabled: bool,
    /// Trae página de ayuda (`F1` la abre en su sección).
    pub has_help: bool,
    /// Cuántos comandos aporta.
    pub commands: u32,
    /// Cuántas columnas aporta.
    pub columns: u32,
    /// Las capabilities que solicita, tal como las declara.
    ///
    /// En la FILA y no solo en la ficha, a propósito: son la decisión que un
    /// humano aprueba, y esconderlas tras un segundo gesto convierte «esto
    /// puede leer tus ficheros» en algo que hay que ir a buscar.
    pub capabilities: Vec<String>,
}

/// Un directorio de extensión que no cargó.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionErrorView {
    /// Dónde, ya saneado.
    pub dir: String,
    /// El texto de arriba DIFIERE de la ruta real.
    pub hostile: bool,
    /// Por qué, ya saneado: lo escribe el core, pero puede citar el
    /// manifiesto del plugin.
    pub reason: String,
}

/// La ficha de una extensión: lo que PIDE y lo que se le ha configurado.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionDetailView {
    /// De quién es esta ficha.
    pub id: String,
    /// Sus claves `[config]` con el valor efectivo. Vacío si no declara
    /// ninguna.
    pub config: Vec<ExtensionConfigRowView>,
}

/// Una clave `[config.<key>]` con su esquema y su valor efectivo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionConfigRowView {
    /// La clave. Charset validado por el manifiesto, segura tal cual.
    pub key: String,
    /// Su tipo (`string`, `bool`, `int`, `enum`). Un tipo que este frontend
    /// no conozca —un peer más nuevo— se pinta como texto y no revienta.
    pub kind: String,
    /// El valor EFECTIVO: los defaults del esquema con el `config.toml`
    /// superpuesto.
    pub value: String,
    /// El valor por defecto del esquema, para poder ver qué se ha cambiado.
    pub default: String,
    /// Qué es, ya enmascarada (texto del manifiesto). Vacía si no lo dice.
    pub description: String,
    /// Los valores válidos de un `enum`, o las cotas de un `int`, ya como
    /// texto. Vacío cuando el tipo no tiene nada que acotar.
    pub domain: String,
}

/// Lo que el visor enseña.
/// Lo que el visor enseña.
/// Lo que el visor enseña.
///
/// Cinco banderas y no un estado: cada una es un HECHO independiente que el
/// host resolvió (es hexadecimal, el encoding lo forzó el usuario, la
/// decodificación tuvo errores, el fichero seguía, el nombre difiere del
/// real), y juntarlas en un enum obligaría a inventar combinaciones que no
/// existen.
///
/// El texto viene DECODIFICADO y en líneas por `norte_frontend::viewer`, que
/// es el mismo modelo que pinta el TUI: la detección de encoding, el salto a
/// hexadecimal de un binario y el recorte de la ventana visible son suyos, no
/// del renderer.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewerView {
    /// El fichero, ya saneado para pintar.
    pub path_display: String,
    /// El texto de arriba DIFIERE del nombre real.
    pub path_hostile: bool,
    /// Nombre del encoding con el que se está leyendo.
    pub encoding: String,
    /// Final de línea detectado (`lf`, `crlf`, `cr`, `mixed`).
    pub eol: String,
    /// Se está enseñando en hexadecimal (binario, o a mano).
    pub hex: bool,
    /// El encoding lo forzó el usuario, no la detección.
    pub forced: bool,
    /// La decodificación tuvo errores: hay bytes que no eran de ese encoding.
    pub had_errors: bool,
    /// Solo se leyó una cabecera: el fichero seguía.
    pub truncated: bool,
    /// Líneas totales de lo leído.
    pub total_rows: u64,
    /// Primera línea visible.
    pub first_line: u64,
    /// Las líneas de la ventana visible, ya saneadas y acotadas.
    pub lines: Vec<String>,
}

/// El reparto de la pantalla: quién se pinta, dónde, y con qué papel.
///
/// Se mide en CELDAS de layout y no en píxeles, que es como están declarados
/// los mínimos de cada panel y como los comparte el TUI: «esto no cabe»
/// significa lo mismo en las dos superficies. El renderer multiplica por el
/// tamaño de su celda —eso sí es suyo— y pinta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutView {
    /// El tamaño que se repartió, en celdas.
    pub cells: (u16, u16),
    /// Los huecos que se pintan, en orden de pintado. Un hueco que no está
    /// aquí es que no cabe o es una pestaña inactiva: no se pinta, y eso lo
    /// decidió el mismo repartidor que usa el TUI.
    pub placements: Vec<SlotPlacement>,
}

/// Un hueco colocado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotPlacement {
    /// Id del hueco.
    pub slot_id: u32,
    /// Columna de la esquina superior izquierda, en celdas.
    pub x: u16,
    /// Fila de la esquina superior izquierda, en celdas.
    pub y: u16,
    /// Ancho en celdas.
    pub width: u16,
    /// Alto en celdas.
    pub height: u16,
    /// Su papel AHORA, si tiene alguno.
    pub role: Option<SlotRole>,
    /// Orden de tabulación. El renderer no lo calcula: mover el foco con el
    /// tabulador es la misma regla en las dos superficies.
    pub focus_index: u32,
}

/// El papel de un hueco.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotRole {
    /// Tiene el foco de teclado.
    Active,
    /// Es el DESTINO de una operación que necesita un segundo sitio.
    Target,
}

/// Estado de la conexión, tal como se pinta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ConnectionView {
    /// Hablando con el daemon.
    Connected,
    /// Se perdió y se está reintentando.
    Reconnecting,
    /// No hay conexión y no se reintenta.
    Lost {
        /// Clave Fluent del motivo.
        reason_key: String,
    },
}

/// Un hueco de la disposición.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SlotView {
    /// Un listado.
    ///
    /// En caja: un listado con su ventana de filas es un orden de magnitud
    /// más grande que un hueco sin proyectar, y un enum que mide lo que su
    /// variante mayor se paga en cada `Vec<SlotView>` que se construye.
    Browser(Box<BrowserSlotView>),
    /// Un hueco de un tipo que este host todavía no proyecta. Se enseña
    /// vacío y con su nombre: preservar lo que no se entiende es la regla de
    /// la sesión (ADR 0059), y desaparecer sería peor que estar en gris.
    Unsupported {
        /// Id del hueco.
        slot_id: u32,
        /// Nombre del kind, para decirlo.
        kind_name: String,
    },
}

/// El listado de un hueco.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserSlotView {
    /// Id del hueco.
    pub slot_id: u32,
    /// Sube en cada re-listado. Una [`RowKey`] de otra generación es vieja.
    pub generation: u64,
    /// La localización, ya saneada para pintar.
    pub path_display: String,
    /// El texto de arriba DIFIERE de la ruta real (bytes no UTF-8, controles
    /// enmascarados). El renderer lo marca; jamás lo esconde.
    pub path_hostile: bool,
    /// Filas del directorio, si se sabe.
    pub total_rows: Option<u64>,
    /// Primera fila que viaja en `rows`.
    pub first_visible: u64,
    /// Las filas de la ventana visible (más el overscan que pida el
    /// renderer). NUNCA el directorio entero.
    pub rows: Vec<RowView>,
    /// Fila bajo el cursor, si hay alguna.
    pub cursor: Option<RowKey>,
    /// Cuántas filas están marcadas en el hueco (no solo en la ventana).
    pub marks: u64,
    /// Las cabeceras de las columnas configuradas, en su orden. Incluye el
    /// nombre, que en las filas viaja aparte (`display_name`).
    pub columns: Vec<ColumnHeader>,
    /// En qué anda el hueco.
    pub state: SlotState,
    /// El buscador incremental, si está abierto. Mientras lo esté, las
    /// teclas de texto son SUYAS: es el contexto de entrada del listado.
    pub quick: Option<QuickView>,
}

/// El buscador incremental de un listado.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickView {
    /// Lo tecleado, ya saneado para pintar.
    pub query: String,
    /// Filtra el listado (`filter`) o salta al primer match (`jump`).
    pub mode: String,
    /// Cuántas filas casan. Con cero, el renderer lo dice: un buscador que
    /// no encuentra nada y no lo enseña parece roto.
    pub matches: u64,
}

/// Lo que le pasa a un listado ahora mismo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum SlotState {
    /// Listado completo y quieto.
    Ready,
    /// Pidiendo la primera página, o rellenando el resto.
    Loading,
    /// El listado falló. La clave Fluent dice por qué; el detalle ya viene
    /// saneado y acotado.
    Error {
        /// Clave Fluent de la categoría.
        reason_key: String,
        /// Detalle ya saneado, si lo hay.
        detail: Option<String>,
    },
}

/// Una fila del listado.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowView {
    /// Clave opaca, válida para esta generación.
    pub key: RowKey,
    /// Nombre listo para pintar.
    pub display_name: String,
    /// El nombre pintado difiere del real: bytes lossy o controles
    /// enmascarados (spec §6). El renderer DEBE marcarlo.
    pub hostile: bool,
    /// Qué es.
    pub kind: RowKind,
    /// Bajo el cursor.
    pub selected: bool,
    /// Marcada para operar.
    pub marked: bool,
    /// Celdas de las columnas configuradas, en el orden de la cabecera.
    pub cells: Vec<CellView>,
}

/// La cabecera de UNA columna.
///
/// La etiqueta viene TRADUCIDA y saneada (`columns::header_label`, la misma
/// que pinta el TUI): un renderer no traduce, y una cabecera de plugin es
/// texto ajeno que ya llega enmascarado.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnHeader {
    /// Id estable de la columna (`name`, `size`, `attr:posix.mode`…). Es lo
    /// que se manda de vuelta para ordenar: el renderer no nombra columnas
    /// por su posición ni por su etiqueta.
    pub id: String,
    /// Etiqueta ya traducida.
    pub label: String,
    /// `asc`/`desc` si el listado se ordena por ESTA columna; `None` si no.
    pub sort: Option<String>,
    /// La columna ordena. Una que no, se pinta sin afordancia de click.
    pub sortable: bool,
}

/// La clase de una entrada, en lo que al pintado le importa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    /// Directorio.
    Dir,
    /// Fichero.
    File,
    /// Enlace simbólico.
    Symlink,
    /// Cualquier otra cosa que el provider reporte.
    Other,
}

/// El valor de una columna, ya formateado.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellView {
    /// Id de la columna a la que pertenece.
    pub column: String,
    /// Texto ya formateado y saneado. `None` = no se sabe (todavía).
    pub text: Option<String>,
}

/// La barra de estado.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StatusView {
    /// Mensaje efímero, ya traducido por el host.
    pub message: Option<String>,
    /// Avisos persistentes (degradación, journal, sesión), acotados.
    pub banners: Vec<String>,
    /// Lo que hay tecleado a medias: una secuencia, un contador, o las dos
    /// cosas. Se pinta SIEMPRE que exista — un prefijo pendiente que no se
    /// ve es un prefijo que no se puede cancelar.
    pub pending: Option<PendingView>,
}

/// Una secuencia o un contador a medio teclear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingView {
    /// Los acordes tecleados, ya pintados (`ctrl+x g`).
    pub chords: String,
    /// El contador acumulado, si el preset los habilita y se está tecleando.
    pub count: Option<u32>,
}

/// Un diálogo abierto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogView {
    /// Su identidad: confirmar dos veces el MISMO id no hace nada dos veces.
    pub id: ModalId,
    /// Clave Fluent del título.
    pub title_key: String,
    /// Líneas de cuerpo, ya saneadas y acotadas.
    pub body: Vec<String>,
    /// Lo que se puede responder.
    pub choices: Vec<DialogChoice>,
    /// El diálogo pide texto libre, y esto es lo tecleado hasta ahora, YA
    /// enmascarado y acotado para pintar. No es el operando: lo que se va a
    /// crear son los bytes que el usuario tecleó, que el host guarda aparte.
    pub input: Option<String>,
    /// Lo tecleado se pinta DISTINTO de lo que es (controles, marcas de
    /// dirección). Es la única superficie donde se pide aprobar un nombre, y
    /// enseñarlo crudo es como se aprueba otra cosa.
    pub input_hostile: bool,
}

/// Una respuesta posible de un diálogo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogChoice {
    /// Id estable de la respuesta (`confirm`, `cancel`, `overwrite`…).
    pub id: String,
    /// Clave Fluent de la etiqueta.
    pub label_key: String,
    /// Esta respuesta DESTRUYE algo: el renderer la pinta como tal.
    pub destructive: bool,
}

/// Una task, tal como se pinta en el tablero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskView {
    /// Id de la task en el daemon.
    pub task_id: u64,
    /// Clase (`copy`, `move`, `delete`, `sync`…).
    pub kind: String,
    /// En qué estado está.
    pub state: TaskStateView,
    /// Porcentaje 0–100 si se sabe.
    pub percent: Option<u8>,
    /// Descripción corta ya saneada (qué se está moviendo).
    pub detail: Option<String>,
    /// La task es de OTRO cliente de la misma sesión.
    pub foreign: bool,
}

/// Estado de una task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStateView {
    /// En cola.
    Queued,
    /// Corriendo.
    Running,
    /// Terminada bien.
    Done,
    /// Falló.
    Failed,
    /// Cancelada.
    Cancelled,
}

/// Un cambio sobre el snapshot anterior.
///
/// `base_sequence` es obligatorio y no es decorativo: aplicar un parche sobre
/// otra base está PROHIBIDO, y el renderer que no tenga esa base pide un
/// snapshot en vez de adivinar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewPatch {
    /// La secuencia sobre la que este parche se aplica.
    pub base_sequence: u64,
    /// Lo que cambia.
    pub changes: Vec<ViewChange>,
}

/// Un cambio concreto.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "change")]
pub enum ViewChange {
    /// El cursor de un hueco se movió (sin re-enviar las filas).
    Cursor {
        /// Hueco.
        slot_id: u32,
        /// Generación en la que vale la clave.
        generation: u64,
        /// Nueva fila bajo el cursor.
        cursor: Option<RowKey>,
    },
    /// Las filas visibles de un hueco cambiaron.
    Rows {
        /// Hueco.
        slot_id: u32,
        /// Generación.
        generation: u64,
        /// Primera fila que viaja.
        first_visible: u64,
        /// Las filas.
        rows: Vec<RowView>,
    },
    /// El estado de un hueco cambió (cargando, error, listo).
    SlotState {
        /// Hueco.
        slot_id: u32,
        /// Estado nuevo.
        state: SlotState,
    },
    /// La barra de estado cambió.
    Status(StatusView),
    /// El tablero de tasks cambió.
    ///
    /// Variante de STRUCT y no de tupla, y no por gusto: un enum etiquetado
    /// por dentro (`tag = "change"`) no puede serializar una variante que
    /// envuelva una secuencia — serde no tiene dónde poner la etiqueta. Como
    /// tupla, esto compilaba y fallaba en tiempo de ejecución en el primer
    /// renderer que lo pidiera por JSON.
    Tasks {
        /// El tablero entero.
        tasks: Vec<TaskView>,
    },
    /// Los diálogos abiertos cambiaron. Struct por el mismo motivo que
    /// [`ViewChange::Tasks`].
    Dialogs {
        /// Los diálogos abiertos, en orden de apertura.
        dialogs: Vec<DialogView>,
    },
    /// La conexión cambió de estado.
    Connection(ConnectionView),
    /// El reparto cambió: la ventana se redimensionó, o el foco (y con él
    /// los papeles) se movió de hueco.
    Layout(LayoutView),
    /// Las cabeceras de un listado cambiaron.
    ///
    /// Ordenar mueve las filas Y la marca de orden. Sin este cambio, tras un
    /// click en la cabecera el listado se repintaba en el orden nuevo y el
    /// `▲` seguía describiendo el anterior: la pantalla se contradecía, y un
    /// lector de pantalla leía `aria-sort` mintiendo.
    Columns {
        /// Hueco.
        slot_id: u32,
        /// Las cabeceras, en su orden.
        columns: Vec<ColumnHeader>,
    },
    /// La paleta se abrió, se filtró, se movió o se cerró.
    Palette {
        /// La paleta, o `None` si se cerró.
        palette: Option<PaletteView>,
    },
    /// El panel de continuaciones apareció, cambió o se fue.
    WhichKey {
        /// Las continuaciones, o `None` si ya no hay prefijo a medias.
        whichkey: Option<WhichKeyView>,
    },
    /// La ayuda se abrió, cambió de página, movió el cursor o se cerró.
    ///
    /// Un parche entero y no un delta por campo: una página cabe de sobra en
    /// un mensaje, y el estado de la ayuda es un todo — la lateral, el
    /// cuerpo y lo ejecutable se mueven juntos cuando el lector abre otra.
    Help {
        /// La ayuda, o `None` si se cerró.
        help: Option<HelpView>,
    },
    /// Las extensiones se abrieron, cambiaron o se cerraron.
    Extensions {
        /// El gestor, o `None` si se cerró.
        extensions: Option<ExtensionsView>,
    },
    /// Los ajustes se abrieron, movieron el cursor o se cerraron.
    Settings {
        /// Los ajustes, o `None` si se cerraron.
        settings: Option<SettingsView>,
    },
    /// El visor cambió (se abrió, se desplazó, se cerró).
    ///
    /// Un parche y no una foto: el visor tapa la pantalla, y mandar el estado
    /// entero por cada línea de scroll enviaba las filas visibles de TODOS
    /// los listados que hay debajo, que es el derroche que la decisión D7
    /// existe para evitar.
    /// Variante de STRUCT, no de tupla: un enum etiquetado por dentro
    /// tampoco puede serializar una variante que envuelva un `Option`. Es la
    /// MISMA trampa que se llevó por delante a `Tasks` y `Dialogs`, y esta
    /// vez la cazó el corpus antes de salir.
    Viewer {
        /// El visor, o `None` si se cerró.
        viewer: Option<ViewerView>,
    },
}

/// Algo que decir que no es un cambio de pantalla.
///
/// Va por la MISMA secuencia que el resto: no es un segundo canal sin orden,
/// porque «se perdió la conexión» y «este listado falló» tienen que llegar en
/// el orden en que pasaron.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "notice")]
pub enum UiNotice {
    /// Un aviso normal, con su clave Fluent.
    Message {
        /// Clave Fluent.
        key: String,
        /// Detalle ya saneado.
        detail: Option<String>,
    },
    /// El host se está apagando y esta es la última cosa que dice.
    Shutdown {
        /// Quedó trabajo sin terminar (una sesión sin escribir, una task
        /// viva). Se DICE, no se calla.
        incomplete: bool,
    },
    /// Un fallo del propio host: el renderer no puede seguir confiando en su
    /// copia del estado. No lleva nombres de fichero ni cuerpos de sesión.
    Fatal {
        /// Clave Fluent del fallo.
        key: String,
    },
}

/// Lo que el host manda al renderer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "update")]
pub enum UiUpdate {
    /// Reemplaza TODO el estado del renderer.
    ///
    /// En caja: una foto entera es un orden de magnitud más grande que un
    /// parche o un aviso, y sin la caja ese tamaño lo paga CADA mensaje que
    /// cruza, la mayoría de los cuales son parches de cursor.
    Snapshot(Box<ViewSnapshot>),
    /// Cambia lo que dice, sobre la base que dice.
    Patch(ViewPatch),
    /// Algo que decir, en el mismo orden que lo demás.
    Notice(UiNotice),
}
