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
//!
//! Y una regla sobre los NÚMEROS, que hoy no cuesta nada y mañana sí (#258).
//! Cada `u64` de este módulo —`RowKey`, `ModalId`, `sequence`, `generation`,
//! `task_id`, `total_rows`, `first_visible`, `marks`, `first_line`— llega al
//! renderer como un `number` de JavaScript, o sea un `f64`: exacto solo hasta
//! 2^53. Todos son contadores pequeños (un índice de fila, una época de
//! listado, el contador del scheduler), así que hoy no hay nada roto. **El
//! día que uno deje de ser un contador pequeño —un hash, un id aleatorio, un
//! valor con la hora dentro— pasa a ser una `String` en el cable ANTES de
//! cambiar de naturaleza**, porque si no el renderer lo redondea y dos filas
//! distintas colisionan sin que nada se ponga rojo. Hacer `RowKey`
//! infalsificable fue considerado y descartado en la ADR 0068; si alguien lo
//! retoma, éste es el párrafo que hay que leer primero.

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
    /// La barra de menús: los títulos, y el desplegado si hay alguno.
    pub menu: MenuView,
    /// La barra de paneles (#324): qué paneles hay, cómo están, y si alguno
    /// tiene algo que contar. Puente 51.
    pub panel_bar: PanelBarView,
    /// La barra de teclas de función (spec 2026-09-10). Puente 63.
    pub key_bar: KeyBarView,
    /// El selector de perfiles, si está abierto.
    pub profiles: Option<ProfilePickerView>,
    /// La paleta de comandos, si está abierta.
    pub palette: Option<PaletteView>,
    /// El asistente de primer arranque (spec 2026-09-10), si está abierto.
    /// Puente 63.
    #[serde(default)]
    pub wizard: Option<WizardView>,
    /// La pantalla de arranque (spec 2026-09-15, ADR 0115), si está puesta.
    /// Puente 69 (con `RowView::progress` y el ritmo de `TaskView`).
    ///
    /// Del HOST y no del webview: lo que la hace valer la pena —dónde estabas,
    /// a dónde sueles ir— solo lo sabe este lado, y una segunda pantalla de
    /// arranque en el renderer acabaría diciendo otra cosa.
    #[serde(default)]
    pub splash: Option<SplashView>,
    /// El panel de continuaciones, si hay un prefijo a medias.
    pub whichkey: Option<WhichKeyView>,
    /// La ayuda, si está abierta. Como el visor, ocupa la pantalla: mientras
    /// esté, las teclas son suyas.
    pub help: Option<HelpView>,
    /// El tema, si se está mirando. Solo LECTURA: se ve qué colores tiene
    /// cada rol y qué efectos declara que este renderer no sabe pintar.
    pub theme: Option<ThemeView>,
    /// Una búsqueda, si hay una abierta.
    pub search: Option<SearchView>,
    /// El panel de diferencias, si hay una comparación abierta.
    pub compare: Option<CompareView>,
    /// El panel de sincronización, si hay un plan abierto.
    pub sync: Option<SyncView>,
    /// El selector de disposiciones, si está abierto.
    pub layouts: Option<LayoutPickerView>,
    /// El selector de COLUMNAS, si está abierto.
    pub columns: Option<ColumnsPickerView>,
    /// Un selector abierto (conexiones o volúmenes), si lo hay.
    pub picker: Option<PickerView>,
    /// Las extensiones, si están abiertas. Desde la 6.4 GOBIERNAN: se
    /// aprueba, se revoca, se enciende, se apaga y se configura — con el
    /// mismo interruptor de efectos que decide si esta ventana escribe.
    pub extensions: Option<ExtensionsView>,
    /// Las sesiones de agente, si el panel está abierto.
    pub agents: Option<AgentsView>,
    /// La salida del último comando de extensión, si sigue en pantalla.
    ///
    /// Fuera del gestor a propósito: un comando se lanza desde la PALETA, y
    /// una salida guardada dentro de una pantalla que no está abierta no la
    /// ve nadie.
    pub plugin_output: Option<ExtensionOutputView>,
    /// La salida de un PROGRAMA que esta ventana corrió esperándolo (#312,
    /// puente 52): hoy, el comparador de dos ficheros. `None` si no hay
    /// ninguna en pantalla.
    pub program_output: Option<ProgramOutputView>,
    /// Los ajustes, si están abiertos. Solo LECTURA: esta ventana enseña lo
    /// que hay y no escribe nada hasta que la fase 5 dé el camino seguro.
    pub settings: Option<SettingsView>,
    /// El visor, si hay uno abierto. Ocupa la pantalla: mientras esté, las
    /// teclas son suyas y el listado no se mueve por debajo.
    pub viewer: Option<ViewerView>,
    /// El plan de renombrado en revisión, si lo hay. Se abre encima del
    /// listado y las teclas son suyas hasta que se apruebe o se descarte.
    pub ai_rename: Option<AiRenameView>,
    /// El plan de ORGANIZAR en revisión (fase 8), si lo hay. Misma forma de
    /// pantalla que el de renombrar y por la misma razón — un documento que
    /// se lee antes de aprobarlo—, con otro contenido: un ÁRBOL.
    pub organize: Option<OrganizeView>,
    /// Idioma negociado, para que el renderer pida el catálogo correcto.
    pub locale: String,
}

/// El selector de PERFILES (ADR 0079).
///
/// Las filas y lo que se dice de cada una son
/// `norte_frontend::profile_picker`, el mismo modelo que pinta el terminal:
/// una fila que no se puede usar se ENSEÑA con su motivo en vez de
/// desaparecer, porque esconder un directorio que el lector creó es peor que
/// enseñarlo roto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfilePickerView {
    /// Las filas, en orden.
    pub rows: Vec<ProfileRowView>,
    /// Cuál está señalada.
    pub cursor: u64,
    /// La generación con la que se pintaron. La lista se llena desde una
    /// tarea de fondo —leer `profiles/` es disco—, así que una fila
    /// nombrada por índice puede nombrar otra cosa (ADR 0068).
    pub generation: u64,
}

/// Una fila del selector de perfiles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileRowView {
    /// El nombre del directorio, ya pintable.
    pub name: String,
    /// Lo pintado difiere de los bytes del directorio (#266).
    pub name_hostile: bool,
    /// Su `[profile] title`, si lo declara. Nunca EN VEZ del nombre: dos
    /// perfiles pueden compartir título y seguir siendo dos.
    pub title: Option<String>,
    /// Es el que está puesto ahora.
    pub active: bool,
    /// Qué OTRA cosa de norte se llama igual, ya dicho en el idioma del
    /// lector. Vacío = solo es un perfil.
    ///
    /// Se avisa porque es una trampa si no se dice: elegir el perfil `far` no
    /// ata ni una tecla del preset `far`.
    pub clash: String,
    /// Este perfil NO puede guardar dónde dejaste cada panel (su nombre no es
    /// UTF-8, D4). Se dice ANTES de elegirlo, no después de perderlo.
    pub no_state: bool,
    /// Por qué no se puede cargar, ya saneado. Vacío = se puede.
    pub problem: String,
}

/// La barra de menús.
///
/// Los menús y sus entradas son `norte_frontend::menu`, el MISMO modelo que
/// pinta el TUI: qué hay en cada menú y en qué orden no se decide dos veces.
/// Lo que se aporta aquí es la proyección — títulos y etiquetas ya traducidos,
/// el atajo de cada entrada, y si esta ventana sabe ejecutarla.
///
/// No añade capacidades: añade una forma de ENCONTRARLAS. La paleta pide que
/// sepas el nombre de lo que buscas y la ayuda pide que leas; un menú se
/// recorre.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuView {
    /// ¿Se pinta la barra? Lo dice `[ui] menu_bar` de la configuración.
    ///
    /// Apagada, la barra no ocupa fila y el menú solo se abre por su tecla —
    /// pero se abre: quien la apaga esconde la barra, no el menú.
    pub bar: bool,
    /// Los títulos, de izquierda a derecha, ya traducidos.
    pub titles: Vec<String>,
    /// Cuál está DESPLEGADO, si alguno. `None` = solo la barra.
    pub open: Option<u64>,
    /// Las entradas del desplegado, vacías si no hay ninguno.
    pub items: Vec<MenuItemView>,
    /// Qué entrada va resaltada dentro del desplegado.
    pub cursor: u64,
}

/// Una entrada de un menú.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuItemView {
    /// La etiqueta CORTA (`menu-item-*`), no la frase de la ayuda: esa es una
    /// descripción, y con ella el desplegable se va a setenta columnas.
    pub label: String,
    /// El atajo que la corre, o `—` si no tiene ninguno en este preset.
    pub chord: String,
    /// Esta ventana puede ejecutarla.
    ///
    /// Una entrada apagada SIGUE saliendo: el menú es el sitio donde se ve
    /// qué existe, y esconder lo que este frontend no hace convertiría una
    /// limitación en un misterio. Es la misma regla que la paleta aplica a
    /// las filas que no puede correr.
    pub enabled: bool,
}

/// La barra de paneles (#324, puente 51): una fila de botones, uno por
/// panel que se abre y se cierra, que ENSEÑA los paneles en vez de esperar a
/// que el lector sepa que existen.
///
/// Qué botones hay y en qué orden lo decide `norte_frontend::panelbar` —el
/// mismo código que la TUI (ADR 0077)—; este host solo recoge el estado y lo
/// traduce. Viaja entera con cada cambio: seis botones no valen un protocolo
/// de deltas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelBarView {
    /// `[ui] panel_bar`: si la barra se pinta. Apagada, los paneles siguen
    /// abriéndose por su tecla, su menú y la paleta.
    pub bar: bool,
    /// `[ui] panel_bar_style = "names"` (spec 2026-09-10): cada botón
    /// enseña su nombre con la letra de acceso marcada; `false` = solo la
    /// letra. Ausente en un host anterior al puente 63 = nombres.
    #[serde(default = "default_true")]
    pub names: bool,
    /// Los botones, en el orden en que se pintan. Un click vuelve como el
    /// ÍNDICE en esta lista (`UiAction::PanelBarActivate`), nunca como un
    /// comando: el renderer no despacha (ADR 0069).
    pub buttons: Vec<PanelButtonView>,
}

/// `true` para un campo que un host anterior no mandaba y que encendido es
/// lo de siempre.
fn default_true() -> bool {
    true
}

/// La barra de teclas de función (spec 2026-09-10, puente 63): diez celdas
/// con lo que cada `F` hace en la pantalla que tiene el teclado.
///
/// DERIVADA del keymap efectivo de esa pantalla —el diálogo si hay uno
/// abierto, el visor si está a pantalla completa, los listados si no—, que
/// es el mismo orden con el que el host elige resolver; por eso dice la
/// verdad. Viaja entera con cada cambio, como la de paneles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBarView {
    /// `[ui] key_bar`: si la barra se pinta.
    pub bar: bool,
    /// Las diez celdas, `F1`..`F10` en orden. Un click vuelve como la TECLA
    /// (`UiAction::KeyBarActivate`), nunca como un comando: el renderer no
    /// despacha (ADR 0069), y una tecla sintetizada va por el mismo camino
    /// que una de verdad.
    pub cells: Vec<KeyCellView>,
}

/// Una celda de la barra de teclas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyCellView {
    /// `1`..=`10`.
    pub key: u32,
    /// La etiqueta corta, en el idioma de la sesión. Vacía = la tecla no
    /// ata nada en esta pantalla, y la celda no se pulsa.
    pub label: String,
    /// El comando que corre, para el título del botón. `None` = nada.
    pub command: Option<String>,
}

/// Un botón de la barra de paneles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelButtonView {
    /// El kind que abre. Texto de disposición, ya enmascarado: un kind puede
    /// venir de un fichero o de un plugin, y acaba en un atributo del DOM.
    pub kind: String,
    /// El nombre corto, en el idioma de la sesión.
    pub label: String,
    /// La letra que la TUI pinta; aquí acompaña a la etiqueta para que las
    /// dos superficies se lean igual.
    pub letter: String,
    /// El atajo que hace lo mismo que el botón, o `—` si no tiene.
    pub chord: String,
    /// Cerrado, abierto, o abierto Y con el teclado.
    pub state: PanelButtonState,
    /// Tiene algo que contar sin estar a la vista: el registro con avisos
    /// sin leer, procesos con tareas en el tablero.
    pub attention: bool,
}

/// Cómo está el panel de un botón.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelButtonState {
    /// Ni siquiera está en la disposición.
    Closed,
    /// Colocado y a la vista, pero el teclado va a otro sitio.
    Open,
    /// Colocado, a la vista, y con el teclado.
    Focused,
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

/// El asistente de primer arranque (spec 2026-09-10, puente 63): un paso,
/// sus filas y el cursor. Todo ya traducido: el renderer pinta y devuelve
/// filas o teclas, y el host escribe lo elegido por su camino de ajustes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WizardView {
    /// `Bienvenido a norte · 1/3 · teclas`.
    pub title: String,
    /// La pregunta del paso.
    pub question: String,
    /// Las filas del paso, en orden. Un click vuelve como el ÍNDICE
    /// (`UiAction::WizardActivateRow`).
    pub rows: Vec<String>,
    /// Cuál está elegida.
    pub cursor: u64,
    /// La línea de teclas.
    pub hint: String,
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
    /// Lo pintado DIFIERE de lo que declara quien aporta la fila.
    ///
    /// Solo puede ser cierto en una fila de PLUGIN: su título y su
    /// descripción los escribe un manifiesto, y esta es la pantalla donde se
    /// elige qué código de tercero correr. Un texto enmascarado que viaja sin
    /// su bandera se lee como fiel.
    pub hostile: bool,
    /// Va arriba por ser de los últimos lanzados (spec 2026-09-10). Solo
    /// con la consulta vacía; con consulta, el orden es el de lo que casa.
    #[serde(default)]
    pub recent: bool,
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
    /// Qué hace, en el idioma del lector. Puede venir de un `keymap.toml`
    /// del usuario, así que va enmascarado.
    pub label: String,
    /// La etiqueta pintada difiere de la que hay en el fichero (#266).
    pub label_hostile: bool,
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
    /// texto para pintar. YA ENMASCARADO.
    pub value: String,
    /// El valor se pinta DISTINTO de lo que es.
    ///
    /// Sale de un `norte.toml` que puede ser el de PROYECTO, y esa capa
    /// significa «he abierto este repositorio», no «doy fe de esta cadena»
    /// (ADR 0026). En la misma lista viven las filas de `PathRowView`, que
    /// siempre tuvieron su bandera: dos clases de fila prometiendo cosas
    /// distintas sobre la misma columna era la incoherencia que había.
    pub hostile: bool,
    /// Cambiarlo pide reiniciar la ventana: lo que se escribe se guarda, y
    /// hace efecto en la siguiente.
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

/// La salida de UN comando de extensión.
///
/// Todo lo de aquí lo escribe un tercero: el texto es lo que el plugin
/// imprimió y el título es el de su manifiesto. Los dos entran enmascarados y
/// acotados, y `truncated` viaja porque el receptor NO puede deducirlo — el
/// texto le llega ya corto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionOutputView {
    /// De qué extensión: su nombre ya enmascarado, con su bandera.
    pub plugin: MaskedTextView,
    /// Su id reverse-DNS, que el core SÍ valida.
    ///
    /// Va con el nombre porque el nombre no identifica: dos extensiones
    /// pueden llamarse igual, y la que dice quién imprimió esto es esta.
    pub plugin_id: String,
    /// Qué comando: su título ya enmascarado, con su bandera.
    pub command: MaskedTextView,
    /// Lo que imprimió, LÍNEA A LÍNEA, cada una enmascarada y acotada.
    ///
    /// Por líneas y no como una cadena: un salto de línea es un control C0,
    /// o sea un peligro de terminal, así que enmascarar la salida entera
    /// marcaba como hostil CUALQUIER salida de más de una línea — una
    /// bandera que es cierta para todo lo honesto no dice nada. Vacío = no
    /// imprimió nada, que se DICE: un panel en blanco se lee como que no
    /// llegó a correr.
    pub lines: Vec<String>,
    /// Alguna línea se pinta distinta de lo que el plugin imprimió.
    pub text_hostile: bool,
    /// La salida no cabía entera y se cortó.
    pub truncated: bool,
}

/// La salida de un PROGRAMA que la ventana corrió y esperó (#312).
///
/// La terminal tiene un camino que el navegador no tiene: suspenderse,
/// correr `diff -u` y esperar una tecla. Esto es su equivalente honesto: el
/// proceso que hospeda corre el programa, captura lo que imprimió y se
/// enseña aquí hasta que el lector lo cierra. Lo que imprimió lo escribió
/// otro programa sobre ficheros que nombró cualquiera: entra enmascarado,
/// por líneas y acotado, como la salida de una extensión.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramOutputView {
    /// Clave Fluent del título: qué se hizo («comparar dos ficheros»).
    pub title_key: String,
    /// El programa y sus argumentos, ya enmascarados, para decir QUÉ corrió.
    pub command: MaskedTextView,
    /// Lo que imprimió (stdout y stderr, en ese orden), LÍNEA A LÍNEA.
    pub lines: Vec<String>,
    /// Alguna línea se pinta distinta de lo que el programa imprimió.
    pub text_hostile: bool,
    /// La salida no cabía entera y se cortó.
    pub truncated: bool,
    /// El programa no pudo correr, o acabó con error. Un comparador
    /// devuelve 1 cuando los ficheros difieren, así que esto NO es
    /// «distinto de cero»: es «no arrancó» o «se pasó del plazo».
    pub failed: bool,
}

/// Las sesiones de AGENTE que esta ventana ha visto pedir permiso.
///
/// Lo que la lista ES va DENTRO de ella (`note`): no hay método en el
/// protocolo que enumere las sesiones vivas, así que esto son las vistas por
/// esta ventana y no el censo de agentes del sistema. Una lista vacía sin esa
/// nota se lee como «ningún agente ha tocado nada», que es una afirmación que
/// esta ventana no puede hacer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsView {
    /// Las sesiones, de la vista más recientemente a la más antigua.
    pub rows: Vec<AgentRowView>,
    /// Cuál está elegida.
    pub cursor: u64,
    /// Cuántas veces ha cambiado esta lista.
    ///
    /// Vuelve con el clic: la lista se reordena SOLA —una petición de
    /// permiso sube a su sesión al primer puesto— y un clic tiene que
    /// resolverse contra la que el lector estaba mirando. Aquí «esta fila» es
    /// de quién se deshace el trabajo.
    pub generation: u64,
    /// Cuántas sesiones se han olvidado por el tope.
    ///
    /// Se dice: el id de sesión lo elige el agente, así que inundar la lista
    /// para empujar fuera a una concreta está a su alcance, y una lista
    /// recortada que se presenta como completa es lo que convierte eso en
    /// «esa sesión no existe».
    pub forgotten: u64,
    /// Qué es esta lista, ya traducido.
    pub note: String,
    /// Qué decir cuando no hay ninguna fila, ya traducido.
    ///
    /// Lo compone el HOST porque no es siempre la misma frase: una ventana de
    /// solo lectura ni siquiera se suscribe al canal de aprobaciones, así que
    /// su lista vacía significa «esta ventana no escucha», no «ningún agente
    /// ha pedido nada» — que es una afirmación que no puede hacer.
    pub empty: String,
}

/// Una sesión de agente vista por esta ventana.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRowView {
    /// Su id, ya enmascarado: es una clave OPACA del daemon y puede llevar
    /// cualquier byte. Lo que viaja de vuelta es el id crudo, no esto.
    pub session: String,
    /// El id se pinta distinto de lo que es.
    pub session_hostile: bool,
    /// Cuántas pidió y cuántas se le aprobaron desde aquí, ya en una frase
    /// traducida.
    ///
    /// Compuesta AQUÍ y no en el renderer: el catálogo que cruza son cadenas
    /// ya traducidas, sin sustitución de variables, así que un `{ $n }` al
    /// otro lado se pinta literal. Y las dos cuentas no son la misma cosa —
    /// otra ventana pudo contestar, o se denegó, o caducó.
    pub counts: String,
    /// Ya se le lanzó un deshacer y sigue en marcha.
    ///
    /// Se dice y además se rehúsa lanzar otro: dos `policy.undo_session` de
    /// la misma sesión caminan la misma lista de entradas, y el segundo
    /// produce un informe lleno de bloqueos que no son de nadie.
    pub undoing: bool,
    /// El último op-kind que pidió (`copy`, `delete`…), ya enmascarado.
    pub last_op: String,
    /// El op-kind se pinta distinto de lo que es.
    pub last_op_hostile: bool,
}

/// Una cadena de tercero lista para pintar, con su bandera al lado.
///
/// Las dos juntas y no en campos hermanos: una bandera suelta acaba
/// describiendo a la cadena de al lado —que es exactamente lo que pasó aquí,
/// donde una sola bandera para tres cadenas la calculaba una de ellas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaskedTextView {
    /// Lo que se pinta, ya enmascarado y acotado.
    pub text: String,
    /// Lo pintado DIFIERE de lo que su autor escribió.
    pub hostile: bool,
}

/// Un comando que aporta una extensión.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionCommandView {
    /// Su id de despacho. NUNCA se pinta: el manifiesto no le valida
    /// charset, así que puede llevar cualquier byte —saltos de línea
    /// incluidos—, y enmascararlo lo rompería como clave.
    pub id: String,
    /// Su título, ya enmascarado y acotado.
    pub title: String,
    /// El título se pinta distinto de lo que declara el manifiesto.
    pub hostile: bool,
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
    /// El MOTIVO se pinta distinto de lo que es.
    ///
    /// Aparte del de `dir` porque son dos cadenas con dos orígenes, y una
    /// sola bandera para las dos deja al lector sin saber cuál mira.
    pub reason_hostile: bool,
}

/// La ficha de una extensión: lo que PIDE y lo que se le ha configurado.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionDetailView {
    /// De quién es esta ficha.
    pub id: String,
    /// Sus claves `[config]` con el valor efectivo. Vacío si no declara
    /// ninguna.
    pub config: Vec<ExtensionConfigRowView>,
    /// Los comandos que aporta, en orden de manifiesto. Vacío si no aporta
    /// ninguno.
    pub commands: Vec<ExtensionCommandView>,
    /// Qué clave está elegida dentro de la ficha.
    pub cursor: u64,
    /// El buffer de edición abierto (`string`/`int`), YA ENMASCARADO. `None`
    /// = no se está editando nada.
    pub editing: Option<String>,
    /// El buffer se pinta distinto de lo que se va a escribir. Un valor de
    /// partida lo escribió el PLUGIN, así que puede traer lo que sea; lo que
    /// viaja de vuelta al daemon es el operando crudo, no esto.
    pub editing_hostile: bool,
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
    /// superpuesto. YA ENMASCARADO.
    pub value: String,
    /// El valor por defecto del esquema, para poder ver qué se ha cambiado.
    /// YA ENMASCARADO.
    pub default: String,
    /// Qué es, ya enmascarada (texto del manifiesto). Vacía si no lo dice.
    pub description: String,
    /// Los valores válidos de un `enum`, o las cotas de un `int`, ya como
    /// texto. Vacío cuando el tipo no tiene nada que acotar.
    pub domain: String,
    /// Alguno de los tres campos de texto libre —valor, defecto, dominio— se
    /// pinta DISTINTO de lo que es.
    ///
    /// Los tres los escribe el plugin en su `plugin.toml` y el manifiesto
    /// solo les acota la LONGITUD, no el charset: un valor de `enum` con un
    /// override bidi dentro llegaba al DOM tal cual mientras tres rustdocs
    /// afirmaban que eso no podía pasar.
    pub hostile: bool,
    /// Este build sabe editar este `kind`.
    ///
    /// `false` para un tipo que no conoce —un peer más nuevo—: el modelo
    /// compartido lo trata como solo lectura, y decirlo evita que la pantalla
    /// ofrezca un `Enter` que no va a cambiar nada.
    pub editable: bool,
}

/// El selector de COLUMNAS: qué columnas hay, en qué orden y con qué formato.
///
/// El modelo es el compartido (`norte_frontend::columns_picker`), que la TUI
/// envuelve en un overlay y esta ventana en un panel: la misma máquina, y por
/// tanto las mismas reglas —el nombre va primero y no se puede ni apagar ni
/// mover, un id que no parsea se PRESERVA porque es intención del usuario, y
/// un attr que el provider anuncia y nadie configuró se OFRECE apagado.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnsPickerView {
    /// Su título, ya traducido, CON el alcance dentro: el esquema al que se
    /// aplica lo elegido (`sftp`, `zip+file`…) o «todos los esquemas».
    ///
    /// El alcance va en el título y no en un campo aparte porque es lo
    /// primero que hay que saber para entender qué se está tocando, y desde
    /// dentro del panel no hay forma de adivinarlo.
    pub title: String,
    /// Las filas, en orden de pintado.
    pub rows: Vec<ColumnsPickerRowView>,
    /// Qué fila tiene el cursor.
    pub cursor: u64,
    /// La frase que explica qué se aplica y qué NO, ya traducida.
    ///
    /// Esta ventana todavía no escribe configuración: lo elegido vale para
    /// ESTA ventana y se pierde al cerrarla. Callarlo dejaría al usuario
    /// creyendo que acaba de configurar norte.
    pub note: String,
    /// El pie con las teclas, pintado desde el KEYMAP (#287).
    ///
    /// Viene del host y no de una cadena del renderer porque los verbos
    /// `dialog.*` se pueden reatar: un pie que dice `Shift+↑/↓` sobre un
    /// keymap que ata otra cosa es una mentira que solo se descubre probando.
    pub hint: String,
}

/// Una fila del selector de columnas.
// Cuatro bools, cada uno un hecho independiente que se pinta distinto: la
// etiqueta difiere de lo real, la columna está encendida, su formato lo fija
// el esquema, y la fila no se puede tocar. Ver `RowView`.
#[expect(
    clippy::struct_excessive_bools,
    reason = "cuatro estados independientes de una celda; ver `RowView`"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnsPickerRowView {
    /// Su id, tal como viaja a la configuración (`size`, `attr:posix.mode`).
    /// Una IDENTIDAD: entera o vacía, jamás recortada.
    pub id: String,
    /// Cómo se llama, ya traducido y saneado. Para un `attr:` o un
    /// `plugin:`, la etiqueta que da su catálogo, que es texto de tercero.
    pub label: String,
    /// La etiqueta se pinta DISTINTA de lo que es.
    pub hostile: bool,
    /// Se pinta en el listado.
    pub enabled: bool,
    /// El formato vigente (`iec`, `iso`…), vocabulario ASCII cerrado. Vacío
    /// = esta columna no admite formato.
    pub format: String,
    /// El formato lo FIJA un ajuste del esquema y aquí no se puede ciclar.
    /// Se pinta apagado en vez de desaparecer: una tecla que no hace nada y
    /// no dice por qué es peor que una que dice que no.
    pub format_locked: bool,
    /// No se puede ni apagar ni mover. Es el caso del NOMBRE, que es la
    /// primera columna por contrato del render.
    pub fixed: bool,
}

/// El tema activo, visto por dentro.
///
/// Los ROLES son la parte compartida: un tema de norte no nombra colores,
/// nombra papeles (`selection`, `error`…), y cada frontend los pinta con su
/// tecnología. Los EFECTOS no: son un bloque libre que interpreta cada
/// renderer, así que lo que esta vista dice de ellos es qué declara el tema y
/// qué de eso sabe hacer ESTA ventana.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeView {
    /// Cómo se llama el tema activo, o el nombre del preset por defecto.
    pub name: String,
    /// Cada rol con su color resuelto (`#rrggbb`), en orden.
    pub roles: Vec<ThemeRoleView>,
    /// Los efectos que el tema declara y que este renderer NO sabe pintar.
    ///
    /// Se dicen, en vez de ignorarse: un tema retro que no se ve distinto es
    /// un tema que el usuario cree roto. Vacío = el tema no declara ninguno.
    ///
    /// Cada clave con su bandera: salen del fichero de tema (#266).
    pub unsupported_effects: Vec<ThemeEffectView>,
    /// Entre qué temas se puede elegir, en orden.
    ///
    /// Esta pantalla ELIGE desde que el catálogo puede volver a cruzar: antes
    /// solo enseñaba, porque lo que hospeda resolvía el tema una vez al
    /// arrancar y no había forma de decirle que había cambiado.
    pub choices: Vec<String>,
    /// Cuál está bajo el cursor. Mover el cursor previsualiza EN VIVO, igual
    /// que en el terminal: un selector de tema que no enseña el tema obliga a
    /// elegir a ciegas.
    pub cursor: u64,
}

/// Un efecto que el tema declara y que este renderer no pinta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeEffectView {
    /// La clave, ya enmascarada.
    pub key: String,
    /// Lo pintado difiere de lo que el fichero dice.
    pub hostile: bool,
}

/// Un rol del tema con su color.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeRoleView {
    /// Qué papel juega (`selection`, `error`…). Vocabulario de norte.
    pub role: String,
    /// Su color, `#rrggbb`. El renderer lo pinta como muestra; no lo parsea
    /// para decidir nada.
    pub color: String,
}

/// El selector de VOLÚMENES del host: elige uno y el panel navega a él.
///
/// Solo volúmenes, hoy. El selector de conexiones que la tarea 4.5 nombra a
/// su lado no está aquí, y la ausencia es una decisión: leer
/// `connections.toml` obliga a meter el crate de conexiones —con russh,
/// opendal, suppaftp, age y el llavero— en esta ventana, para una lista que
/// todavía no puede abrir ninguna. Llega con la fase 5, que necesita ese
/// crate de todas formas. Mientras tanto `pane.connect` contesta «aquí no»,
/// que es verdad.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickerView {
    /// Su título, ya traducido.
    pub title: String,
    /// Las filas.
    pub rows: Vec<PickerRowView>,
    /// Cuál está elegida, si hay alguna.
    pub cursor: Option<u64>,
    /// La lista está vacía y por qué, ya traducido. Vacío cuando hay filas.
    ///
    /// «No hay ninguna» y «todavía no ha contestado» no son lo mismo, y una
    /// lista vacía sin frase se lee siempre como lo primero.
    pub empty: String,
    /// Sube cada vez que cambia el CONJUNTO de filas.
    ///
    /// El selector de volúmenes se abre VACÍO y se llena cuando contesta el
    /// daemon, así que tiene la misma carrera que la barra lateral: un click
    /// pintado sobre una lista y atendido sobre otra. Ver
    /// [`PlacesSlotView::generation`].
    pub generation: u64,
}

/// Una fila de un selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickerRowView {
    /// Lo que se enseña, ya saneado.
    pub label: String,
    /// El texto de arriba DIFIERE de lo real (un punto de montaje es BYTES).
    pub hostile: bool,
    /// El detalle de la derecha, ya saneado: la URL de una conexión, o el
    /// sistema de ficheros y el espacio de un volumen.
    pub detail: String,
}

/// La hoja de atributos de una entrada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataSlotView {
    /// Id del hueco.
    pub slot_id: u32,
    /// Los campos, en orden: primero los que tiene toda entrada, luego los
    /// atributos que el provider trajo con el listado.
    pub fields: Vec<MetadataFieldView>,
    /// No hay nada que enseñar, y esta es la frase que lo dice (el panel al
    /// que sigue está vacío). Vacía cuando sí hay campos.
    pub note: String,
    /// La ruta del listado al que esta hoja SIGUE, ya pintable.
    ///
    /// «Detalles» a secas no dice de qué son los detalles: con dos listados
    /// abiertos no había forma de saber cuál se está describiendo salvo mover
    /// el cursor y mirar si la hoja se movía. Viaja aparte de los campos
    /// porque no describe a la ENTRADA sino al panel, y va en el título.
    ///
    /// Vacía si el vínculo no resuelve a ningún listado.
    pub follows_display: String,
    /// La ruta de arriba DIFIERE de los bytes reales.
    pub follows_hostile: bool,
}

/// Un campo de la hoja.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataFieldView {
    /// Cómo se llama, ya traducido (o la cabecera del catálogo de atributos).
    pub label: String,
    /// Su valor, ya formateado y saneado.
    pub value: String,
    /// El valor DIFIERE de lo real (solo el nombre puede serlo).
    pub hostile: bool,
}

/// El panel de árbol de directorios.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeSlotView {
    /// Id del hueco.
    pub slot_id: u32,
    /// Las ramas visibles, en orden de pintado.
    pub rows: Vec<TreeRowView>,
    /// Qué fila tiene el cursor.
    pub cursor: u64,
    /// Sube cada vez que cambia el CONJUNTO de filas.
    ///
    /// Y cambia solo: desplegar una rama pide su listado, y ese listado llega
    /// de una task de fondo e inserta filas EN MEDIO. Entre que el lector
    /// suelta el botón sobre una y el host atiende la acción, esa fila puede
    /// ser otra — el mismo peligro que la barra de sitios, y la misma cura
    /// (ADR 0068).
    pub generation: u64,
}

/// Una rama del árbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeRowView {
    /// El nombre del directorio, saneado. La raíz lleva su ruta entera:
    /// «`/`» a secas no dice desde dónde cuelga esto.
    pub label: String,
    /// El nombre PINTADO difiere de los bytes reales.
    pub hostile: bool,
    /// Cuántos niveles por debajo de la raíz (la raíz es 0).
    pub depth: u32,
    /// Está desplegada.
    pub expanded: bool,
    /// Tiene hijos que enseñar. `None` = todavía no se ha mirado, y son tres
    /// estados distintos para el lector: una rama que se puede abrir, una hoja
    /// que no, y una que aún no se sabe. Pintar «hoja» a algo que no se ha
    /// leído es una respuesta inventada.
    pub children: Option<bool>,
}

/// La barra lateral de sitios.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacesSlotView {
    /// Id del hueco.
    pub slot_id: u32,
    /// Sus filas, en orden: cabecera de unidades, las unidades, cabecera de
    /// favoritos, los favoritos. Una sección plegada no lista las suyas, pero
    /// su cabecera SIGUE: sin ella la lista da un brinco cuando llegan.
    pub rows: Vec<PlaceRowView>,
    /// Qué fila tiene el cursor.
    pub cursor: u64,
    /// Sube cada vez que cambia el CONJUNTO de filas.
    ///
    /// Sin esto un click no era seguro. Los volúmenes llegan de una tarea de
    /// fondo y se insertan EN MEDIO de la lista —las unidades van antes que
    /// los favoritos—, así que entre que el usuario suelta el botón sobre
    /// `~/proyectos` y el host atiende la acción, esa fila puede ser `/boot`.
    /// El índice viaja acompañado de la generación con la que se pintó, y una
    /// que no case se rechaza en vez de navegar a otro sitio (ADR 0068).
    pub generation: u64,
}

/// Una fila de la barra lateral.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "row")]
pub enum PlaceRowView {
    /// La cabecera de una sección. No navega.
    Header {
        /// Su texto, ya traducido.
        label: String,
        /// Está plegada.
        folded: bool,
    },
    /// Un volumen del host.
    Drive {
        /// Cómo se llama: su etiqueta si la tiene, o su punto de montaje. Ya
        /// saneado — ninguna plataforma promete que una etiqueta sea UTF-8.
        label: String,
        /// El texto de arriba DIFIERE de lo real.
        hostile: bool,
        /// El espacio y si es de solo lectura, ya formateado. Un tamaño que
        /// el sistema no contestó se DICE; jamás se pinta un `0`.
        detail: String,
    },
    /// Un favorito de la hotlist.
    Favorite {
        /// El nombre que le puso el usuario, ya saneado.
        name: String,
        /// A dónde va, ya saneado. Vacío si su ruta no parsea.
        target: String,
        /// El texto de arriba DIFIERE de lo real.
        hostile: bool,
        /// Su ruta no parsea, y esta es la razón ya traducida. Vacía cuando
        /// el favorito está bien.
        ///
        /// Un favorito roto se PINTA con su motivo: uno que desaparece en
        /// silencio es un fallo de configuración que nadie puede ver.
        broken: String,
    },
}

/// El selector de disposiciones, con la vista previa de la elegida.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutPickerView {
    /// Su título, ya traducido.
    pub title: String,
    /// Las filas: primero las cinco de fábrica, luego las del usuario.
    pub rows: Vec<LayoutRowView>,
    /// Cuál está elegida.
    pub cursor: u64,
    /// La FORMA de la disposición elegida, en caracteres: una línea por fila
    /// de la miniatura, todas del mismo ancho.
    ///
    /// La pinta el host con el mismo motor que reparte la pantalla de verdad,
    /// así que la vista previa no puede mentir sobre lo que va a salir.
    pub preview: Vec<String>,
    /// Por qué la elegida no tiene vista previa, ya traducido. Vacío cuando
    /// sí la tiene.
    ///
    /// CITA el fichero del usuario (el diagnóstico del parser TOML), así que
    /// va enmascarado y con su bandera: lo que se enmascara se dice (#266).
    pub problem: String,
    /// El diagnóstico pintado difiere de lo que el fichero contiene.
    pub problem_hostile: bool,
}

/// Una disposición ofrecida.
///
/// Cuatro banderas y no un estado: cada una es un HECHO independiente —de
/// fábrica, el nombre difiere del real, comparte nombre con un preset de
/// teclado, su fichero no parsea— y juntarlas en un enum obligaría a
/// inventar combinaciones que no existen.
#[expect(
    clippy::struct_excessive_bools,
    reason = "avisos independientes de una fila; un enum inventaría combinaciones"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutRowView {
    /// Su nombre, ya saneado. El nombre REAL son bytes —acaba en
    /// `layouts/<nombre>.toml`— y no viaja: para elegir una fila se manda su
    /// índice, no su nombre.
    pub name: String,
    /// El texto de arriba DIFIERE del nombre real.
    pub hostile: bool,
    /// Es una de las de fábrica.
    pub factory: bool,
    /// Su nombre coincide con el de un preset de TECLADO, y elegirla no
    /// cambia ni una tecla. Se avisa: sin la línea, la coincidencia es una
    /// trampa en vez de una comodidad.
    pub shares_keymap_name: bool,
    /// Su fichero no parsea.
    pub broken: bool,
}

/// Una búsqueda por el subárbol, con lo que lleva encontrado.
///
/// Los resultados llegan en LOTES mientras la búsqueda corre: la vista se
/// puede recorrer y usar antes de que termine, que es la mitad del valor de
/// buscar en un árbol grande.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchView {
    /// Lo que se buscó, ya saneado.
    pub query: String,
    /// Dónde, ya saneado.
    pub root: String,
    /// El texto de arriba DIFIERE de la ruta real.
    pub root_hostile: bool,
    /// Lo encontrado hasta ahora.
    pub rows: Vec<SearchRowView>,
    /// Cuál está elegida, si hay alguna.
    pub cursor: Option<u64>,
    /// Se preguntó por SIGNIFICADO contra el índice, no por nombre contra el
    /// árbol.
    ///
    /// El renderer lo necesita para dos cosas: titular la vista y decidir si
    /// pinta la columna de parecido. Y para no prometer lo que no hay: una
    /// búsqueda semántica no recorre un subárbol, así que su alcance es el
    /// índice entero y no [`Self::root`].
    pub semantic: bool,
    /// En qué estado está, YA dicho: cuántos van y si sigue corriendo, si
    /// terminó, o si paró en su tope.
    ///
    /// Compuesto en Rust con la MISMA familia de frases que usa el TUI
    /// (`search-status-*`): el catálogo llega al renderer con los textos ya
    /// resueltos, así que interpolar un número es cosa del host.
    ///
    /// Los tres estados se dicen distinto porque son distintos: una lista
    /// corta que ya no crece, una que todavía crece y una que paró en el tope
    /// se leen igual si nadie las nombra.
    pub status: String,
    /// Sigue corriendo. Va aparte de [`Self::status`] porque el renderer lo
    /// usa para pintar, no para leer.
    pub running: bool,
}

/// Un resultado.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchRowView {
    /// El nombre del fichero, ya saneado.
    pub name: String,
    /// El texto de arriba DIFIERE del nombre real.
    pub hostile: bool,
    /// Dónde está, ya saneado: el directorio que lo contiene.
    pub parent: String,
    /// El directorio de arriba DIFIERE del real.
    pub parent_hostile: bool,
    /// Es un directorio.
    ///
    /// `false` también cuando NO se sabe: un hallazgo semántico trae ruta y
    /// parecido, no clase, y activarlo abre la carpeta con el cursor encima
    /// —que es lo que hay que hacer con un fichero— en vez de intentar
    /// entrar en algo que puede no ser un directorio.
    pub is_dir: bool,
    /// Cuánto se parece a lo que se preguntó, en `[-1, 1]`, mayor = más.
    ///
    /// `None` en una búsqueda por NOMBRE: ahí no hay grados, o el patrón casa
    /// o no casa, y pintar un número inventado convertiría un orden de
    /// llegada en un ranking.
    pub score: Option<f64>,
}

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
#[expect(
    clippy::struct_excessive_bools,
    reason = "el visor: hex, recorte y ventana son suyos, no del renderer"
)]
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
    /// Ancho de la línea más larga, en CELDAS.
    ///
    /// Con `first_col`, es lo que el renderer necesita para dibujar barra
    /// horizontal. `0` en hexadecimal, que tiene un ancho fijo y no se
    /// desplaza. Viaja aunque las `lines` ya vengan recortadas: el recorte dice
    /// qué se ve, y esto dice cuánto hay — sin lo segundo, un fichero cortado
    /// por la derecha se lee como un fichero corto.
    ///
    /// Sin `serde(default)`, como el resto de esta vista: la compatibilidad
    /// hacia atrás la resuelve `bridge_version` en el sobre, y un `default`
    /// aquí solo debilitaría el golden — si alguien dejara de serializarlos, el
    /// round-trip pasaría con ceros.
    pub total_cols: u64,
    /// Primera columna visible, en celdas.
    pub first_col: u64,
    /// Las líneas de la ventana visible, ya saneadas y acotadas.
    pub lines: Vec<String>,
    /// «via ‹plugin›», ya traducido y con el nombre enmascarado dentro. Vacío
    /// = es el fichero, leído por norte.
    ///
    /// Se dice siempre que hay uno. Un previewer puede enseñar cualquier cosa
    /// —es su trabajo: un PDF como texto, un JSON formateado— y quien mira
    /// tiene derecho a saber que no está viendo los bytes del fichero.
    ///
    /// Traducido aquí porque interpola el nombre, y un renderer no traduce.
    pub preview_by: String,
    /// La decodificación del fichero que se le dio al previewer fue con
    /// PÉRDIDA: los `�` de su salida vienen de ahí y no del fichero.
    ///
    /// Aparte de `had_errors`, que es el de la vista cruda: son dos
    /// decodificaciones distintas y confundirlas culpa al fichero de lo que
    /// hizo la lectura.
    pub preview_lossy: bool,
    /// Esto es una IMAGEN que se puede pintar, y así de grande dice ser.
    ///
    /// `None` = no es una imagen, o es una que esta ventana se NIEGA a
    /// pintar; en el segundo caso [`Self::image_refused`] dice por qué. El
    /// renderer pide los bytes aparte —no viajan en la foto— y hasta que
    /// llegan enseña la vista cruda.
    pub image: Option<ImageView>,
    /// Por qué NO se va a pintar una imagen que sí se reconoció, ya
    /// traducido. Vacío = no hay nada que explicar.
    ///
    /// Se dice en vez de caer en silencio al hexview: un fichero que el
    /// usuario sabe que es una foto y que aparece como bytes sin una palabra
    /// parece norte roto, no norte prudente.
    pub image_refused: String,
    /// Las líneas visibles CON ESTILO cuando lo que se enseña lo produjo un
    /// previewer (puente 49): una entrada por fila de [`Self::lines`], cada
    /// una la lista ordenada de sus fragmentos. Vacío en la vista cruda.
    ///
    /// El mismo texto que `lines`, partido y con su rol o su color: la TUI
    /// lo pintaba desde el primer día y la ventana lo aplanaba. Un renderer
    /// que no pinte fragmentos sigue con `lines` y no pierde nada.
    pub styled: Vec<Vec<SpanView>>,
}

/// Un fragmento de una línea de preview con estilo (ADR 0037).
///
/// `role` GANA sobre `fg` cuando vienen los dos, como en la TUI: el tema del
/// lector manda sobre el color fijo de un plugin. Un rol que el tema no
/// conoce no llega aquí: el modelo compartido ya lo dejó en `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanView {
    /// El texto, ya enmascarado a la entrada y acotado aquí.
    pub text: String,
    /// El rol del tema en kebab-case (`title`, `error`, `match`…), validado.
    pub role: Option<String>,
    /// El color propio del plugin, `#rrggbb`. Solo cuenta sin `role`.
    pub fg: Option<String>,
    /// El FONDO del fragmento, `#rrggbb` (puente 50): un previewer de imagen
    /// pinta medios bloques con el píxel de arriba en `fg` y el de abajo
    /// aquí. Ningún rol manda sobre él.
    pub bg: Option<String>,
}

/// Una imagen reconocida y aceptada: qué es y cuánto dice medir.
///
/// Lo que declara su CABECERA, no lo que mida de verdad — nadie la ha
/// decodificado todavía, y ese es justo el punto: el tamaño declarado es lo
/// que se compara con el presupuesto ANTES de dársela a un decodificador.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageView {
    /// Su formato, reconocido por bytes MÁGICOS y jamás por la extensión: una
    /// extensión es una afirmación de quien nombró el fichero.
    pub format: String,
    /// Ancho declarado, en píxeles.
    pub width: u32,
    /// Alto declarado, en píxeles.
    pub height: u32,
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
    /// Las PESTAÑAS de cada grupo que hay en pantalla.
    ///
    /// Aparte de los `placements` porque una pestaña inactiva NO se coloca —
    /// no se pinta su contenido— y aun así hay que enseñar que está: una
    /// ventana con tres pestañas que solo muestra la de delante y no dice que
    /// hay otras dos es una ventana que esconde trabajo abierto.
    pub tabs: Vec<TabGroupView>,
    /// Si la marca de DESTINO dice algo con los listados que hay a la vista.
    ///
    /// Que el rol EXISTA y que se PINTE son dos preguntas. La primera la
    /// contesta [`SlotPlacement::role`], que es el modelo; ésta es la
    /// segunda, y viaja calculada porque la decide el crate compartido
    /// (`layout::target_worth_marking`) y no el renderer: escrita allí era un
    /// número repetido en TypeScript, o sea la misma decisión en dos sitios
    /// que esta rama existe para dejar de tener (ADR 0077).
    ///
    /// Con dos listados el destino es «el otro» y nadie necesita que se lo
    /// digan; una marca que sale siempre deja de leerse, y entonces no está
    /// el día que hay tres y una copia hacia el que el motor desempate solo
    /// es pérdida de datos silenciosa (ADR 0058 D7).
    ///
    /// `#[serde(default)]`: ausente = `false`, que es no marcar. La dirección
    /// segura, porque la marca de más es la que enseña a ignorarla.
    #[serde(default)]
    pub mark_target: bool,
}

/// Un grupo de pestañas y cuál está delante.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabGroupView {
    /// El hueco COLOCADO al que pertenece este grupo: el de la pestaña
    /// activa, que es el que el renderer está pintando.
    pub slot_id: u32,
    /// Sus pestañas, en el orden del árbol.
    pub tabs: Vec<TabView>,
    /// Cuál está delante, como índice en `tabs`.
    pub active: u64,
}

/// Una pestaña.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabView {
    /// El hueco que hay dentro. Es lo que vuelve al elegirla con el ratón.
    pub slot_id: u32,
    /// Su rótulo: el nombre del directorio de su listado, ya enmascarado —un
    /// directorio con nombre hostil dentro de una pestaña es tan hostil como
    /// dentro de un listado—. Para lo que no es un listado, el nombre de su
    /// kind.
    pub title: String,
    /// El rótulo se pinta distinto de lo que es.
    pub title_hostile: bool,
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
    /// La hoja de atributos: lo que el listado ya sabe de la entrada bajo el
    /// cursor del panel al que este hueco sigue.
    ///
    /// **No lee nada.** La `Entry` ya está en el listado, y un panel que
    /// siguiera al cursor pidiendo datos por fila convertiría bajar por un
    /// directorio en una tormenta de peticiones.
    Metadata(Box<MetadataSlotView>),
    /// La barra lateral de sitios: los volúmenes del host y los favoritos
    /// del usuario, con su cursor.
    Places(Box<PlacesSlotView>),
    /// El árbol de directorios: qué ramas hay abiertas y cuál tiene el cursor.
    ///
    /// **Solo directorios**, y **perezoso**: desplegar una rama lista ESE
    /// directorio y nada más. Un árbol que se leyera entero al abrirse tardaría
    /// minutos en un `$HOME` grande y horas contra un remoto.
    Tree(Box<TreeSlotView>),
    /// El panel de procesos: las MISMAS tareas que pinta la franja, con su
    /// propio cursor.
    ///
    /// No guarda una segunda copia: dos listas de tareas se separan, y la que
    /// se ve deja de ser la que se cancela.
    Processes {
        /// Id del hueco.
        slot_id: u32,
        /// Qué fila tiene el cursor, si hay alguna.
        cursor: Option<u64>,
    },
    /// El panel de registro: lo que este proceso está registrando (#326).
    Log(Box<LogSlotView>),
    /// El visor ACOPLADO (#291, puente 51): el fichero bajo el cursor del
    /// listado al que este hueco sigue, leído solo. Kind `viewer` en la
    /// disposición; `preview` en el wire, que es lo que es.
    Preview(Box<PreviewSlotView>),
    /// El panel que pinta un PLUGIN (fase 3): el marco que describió su guest.
    Panel(Box<PanelSlotView>),
    /// El mapa de disco (fase 4): de qué está hecho el directorio, repartido
    /// en rectángulos.
    ///
    /// El reparto lo hace el HOST con `norte_frontend::treemap::squarify`, no
    /// el renderer: un treemap calculado dos veces son dos treemaps distintos
    /// en cuanto alguien toque un redondeo (ADR 0077). Lo que cruza son las
    /// líneas ya estiladas y sus zonas, igual que un panel de plugin.
    DiskMap(Box<DiskMapSlotView>),
    /// Un hueco de un tipo que este host todavía no proyecta. Se enseña
    /// vacío y con su nombre: preservar lo que no se entiende es la regla de
    /// la sesión (ADR 0059), y desaparecer sería peor que estar en gris.
    Unsupported {
        /// Id del hueco.
        slot_id: u32,
        /// Nombre del kind, para decirlo. Lo escribe la disposición del
        /// usuario, así que va enmascarado.
        kind_name: String,
        /// El nombre pintado difiere del que hay en el fichero (#266).
        kind_name_hostile: bool,
    },
}

/// El visor acoplado (#291): lo que enseña un hueco `viewer`.
///
/// El MISMO [`ViewerView`] que el visor a pantalla completa —es el mismo
/// visor en otro sitio, como en la TUI—, con dos diferencias que son del
/// vínculo y no del contenido: sigue al cursor del listado en vez de abrirse
/// con una tecla, y las líneas vienen ENTERAS hasta el tope del puente para
/// que el hueco las desplace solo, porque no tiene teclas de visor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviewSlotView {
    /// Id del hueco.
    pub slot_id: u32,
    /// El visor con lo leído, o `None` si no hay fichero que enseñar.
    pub viewer: Option<ViewerView>,
    /// Por qué no hay fichero, YA DICHO: un directorio, nada bajo el
    /// cursor, un error de lectura. Vacío cuando hay visor.
    pub note: String,
}

/// El panel que pinta un PLUGIN (fase 3): lo que su guest describió.
///
/// El guest no dibuja, DESCRIBE: líneas con estilo y zonas pulsables. El
/// borde, el título y el foco los pone la ventana, que es lo que impide que un
/// plugin se haga pasar por otro panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelSlotView {
    /// Id del hueco.
    pub slot_id: u32,
    /// Qué panel es: el `<kind>`, sin el prefijo, ENMASCARADO y acotado.
    ///
    /// Al declararlo se le exige un alfabeto (`KindRegistry::insert_panels`),
    /// pero esto no sale de ahí: sale del ÁRBOL, que puede venir de un fichero
    /// de disposición o de la sesión, y a un kind escrito a mano no le ha
    /// exigido nada nadie. Se trata como el nombre de cualquier kind que el
    /// host no conoce.
    pub title: String,
    /// Las líneas del marco, cada una con sus tramos. Vacío mientras el primer
    /// marco no ha llegado, o si el plugin falló: el hueco se pinta con su
    /// borde y nada dentro, nunca en blanco sin marco.
    pub lines: Vec<Vec<SpanView>>,
    /// Las zonas pulsables, en celdas DENTRO del marco.
    pub hits: Vec<HitView>,
}

/// El mapa de disco (fase 4): el treemap ya repartido, listo para pintar.
///
/// Mismo reparto que un panel de plugin —líneas estiladas y zonas en celdas
/// DENTRO del marco— y por la misma razón: el renderer pinta lo que le den y
/// dice DÓNDE se pulsó; quién es cada rectángulo lo resuelve el host contra su
/// propio marco.
///
/// Aquí eso pesa más que allí, porque lo que se resuelve es el NOMBRE de un
/// fichero: mandarlo por el cable obligaría a elegir entre la forma que se
/// pinta —enmascarada, que no identifica nada— y la reversible, y sería un
/// nombre que puede mandar cualquiera que hable con el renderer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiskMapSlotView {
    /// Id del hueco.
    pub slot_id: u32,
    /// Qué directorio se está describiendo, para el título. Enmascarado y
    /// acotado: sale de un nombre de fichero.
    pub title: String,
    /// El nombre pintado difiere del que hay en el disco (#266).
    pub title_hostile: bool,
    /// Las líneas del treemap, cada una con sus tramos. Vacío mientras no se
    /// haya medido nada: el hueco se pinta con su borde y nada dentro.
    pub lines: Vec<Vec<SpanView>>,
    /// Un rectángulo por zona, en celdas DENTRO del marco.
    pub hits: Vec<HitView>,
    /// La medida sigue en marcha.
    ///
    /// Viaja porque un mapa a medias sin decirlo se lee como un directorio
    /// pequeño, que es la respuesta equivocada y encima creíble.
    pub measuring: bool,
}

/// Una zona pulsable de un panel de plugin: dónde está, y nada más.
///
/// **Sin su comando, a propósito.** El renderer dice DÓNDE se pulsó y el host
/// resuelve qué zona era y qué comando le toca, con el mismo filtro que el
/// terminal. Es la regla de esta ventana —el renderer cuenta lo que pasó, el
/// host decide qué significa—, y aquí además cierra una puerta: un comando que
/// viajara por el cable sería un comando que puede mandar cualquiera que hable
/// con el renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HitView {
    /// Fila dentro del marco, contando desde cero.
    pub row: u16,
    /// Columna donde empieza.
    pub col: u16,
    /// Cuántas celdas ocupa a lo ancho.
    pub width: u16,
}

/// El panel de registro (#326): la ventana visible del anillo en memoria.
///
/// Solo la VENTANA, como el listado: un anillo de dos mil líneas mandado entero
/// en cada parche es el derroche que la decisión D7 existe para evitar, y el
/// registro se mueve más que un directorio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogSlotView {
    /// Id del hueco.
    pub slot_id: u32,
    /// Las líneas visibles, de arriba abajo y ya saneadas.
    pub lines: Vec<LogLineView>,
    /// Hasta qué nivel se está ENSEÑANDO, en su forma de wire.
    ///
    /// Vocabulario cerrado (`error`, `warn`, `info`, `debug`, `trace`) y no la
    /// etiqueta traducida: el renderer marca cuál está puesto, y comparar
    /// frases traducidas para eso obligaría al renderer a conocer el idioma
    /// del host.
    pub level: String,
    /// Ese mismo nivel tal y como se PINTA (`TRACE`). Ver
    /// [`LogLineView::level_label`]: el de arriba se compara, éste se lee.
    #[serde(default)]
    pub level_label: String,
    /// El filtro de texto vigente, enmascarado y acotado. Vacío = todo.
    ///
    /// Lo teclea el lector, así que puede traer controles y marcas de
    /// dirección: es texto para pintar como cualquier otro.
    pub filter: String,
    /// Está pegado al final y sigue lo que llega.
    ///
    /// Se dice porque es la diferencia entre «no pasa nada» y «te has
    /// despegado y esto es historia»: sin ello, un panel quieto durante una
    /// operación larga se lee igual en los dos casos.
    pub following: bool,
    /// Cuántas líneas pasan el filtro, para poder situar la ventana.
    pub total: u64,
    /// Índice de la primera línea que viaja en `lines`, dentro de las
    /// filtradas.
    pub first_visible: u64,
    /// Cuántas líneas se han perdido, y de QUÉ anillo, ya DICHO.
    ///
    /// Se dice: un registro con un agujero silencioso miente sobre lo que
    /// pasó, y la ausencia de una línea es indistinguible de que el evento no
    /// ocurriera.
    ///
    /// Con las dos fuentes a la vista (#328) son **dos números y no uno**,
    /// cada uno nombrando su anillo, porque no significan lo mismo ni viven lo
    /// mismo: el de la ventana cuenta lo que su anillo ha evacuado desde que
    /// arrancó el proceso y no se reinicia nunca; el del daemon cuenta lo que
    /// ESTA apertura del panel se perdió. Sumarlos daba un número que no era
    /// ninguna de las dos cosas.
    ///
    /// Traducido aquí y con el NÚMERO dentro, no un `u64` para que el renderer
    /// componga la frase: un renderer no traduce ni sustituye números. Es la
    /// misma regla que `BrowserSlotView::skipped_note`. Vacío = ninguna.
    pub dropped_note: String,
    /// Qué anillo está CAPTURANDO más de lo que se enseña, y hasta dónde. Ya
    /// traducido; vacío = ninguno.
    ///
    /// Existe porque los dos niveles se separan a propósito —bajar lo que se
    /// enseña no deja de capturar, o volver a subir mostraría un agujero— y
    /// entonces el panel puede decir «info» mientras el proceso guarda TRACE
    /// en memoria. Quien mira tiene derecho a saber que se está recogiendo más
    /// de lo que ve, sobre todo antes de hacer una captura de pantalla.
    ///
    /// Y desde #328 es también donde se dice el nivel del DAEMON, nombrándolo:
    /// el suyo es global a todos sus clientes, otro pudo subirlo y nunca baja,
    /// así que puede estar muy por encima del que este panel enseña. En
    /// [`Self::level`] no cabe —ése es el que FILTRA la lista y el que los
    /// botones mueven— y ponerlo ahí dejaba marcado un nivel que el panel no
    /// estaba aplicando.
    pub capturing: String,
    /// De qué PROCESO son estas líneas, ya traducido.
    ///
    /// Existe porque en la ventana la respuesta no es obvia y además no es la
    /// que uno espera: `norte-gui` arranca su propio daemon (#300), así que
    /// este anillo lleva lo del proceso de la VENTANA y **no** lo del daemon,
    /// que es donde pasa la mitad interesante —los providers, el journal, la
    /// política—. En la TUI embebida son el mismo proceso y no se nota.
    ///
    /// Callarlo haría que el panel pareciera roto: alguien abre el registro
    /// mientras una conexión falla, no ve la línea que lo explica, y concluye
    /// que el panel no funciona en vez de que está mirando otro proceso.
    /// Desde #328 las del daemon también llegan, y esto dice cuáles se ven.
    pub source: String,
    /// La fuente EFECTIVA, en vocabulario cerrado: `window`, `daemon` o
    /// `both` (#328).
    ///
    /// Efectiva y no la preferencia guardada: sin un segundo anillo al otro
    /// lado —un daemon sin la feature `logging`— la preferencia `both` se
    /// enseña como `window`, porque eso es lo que el lector está mirando. Un
    /// panel que dijera «los dos» sobre las líneas de uno solo mentiría en el
    /// sitio donde más caro sale: el que abre el registro buscando lo que no
    /// encuentra.
    ///
    /// Cerrado y sin traducir, como `level`: el renderer marca cuál está
    /// puesta, y comparar frases traducidas para eso lo ataría al idioma.
    pub source_mode: String,
    /// Hay de verdad una SEGUNDA fuente que ofrecer.
    ///
    /// `false` mientras el daemon no haya contestado nunca a su registro, y
    /// entonces el selector no se pinta: ofrecer tres fuentes donde solo hay
    /// una es un mando que no hace nada, que es peor que no tenerlo.
    pub sources_available: bool,
    /// Lo que hay que decir sobre la fuente, ya traducido. Vacío = nada.
    ///
    /// Dos frases, y son excluyentes. Que el daemon **no tiene registro que
    /// servir**, que es la mitad de #326 aplicada a la otra orilla: el panel
    /// vuelve al anillo local y lo dice, en vez de quedarse mudo. Y, cuando lo
    /// que se enseña es el del daemon, **de quién es el nivel**: es global al
    /// proceso, otro cliente pudo subirlo, y solo sube — así que el número que
    /// hay al lado no es «lo que pediste», y callarlo dejaría al lector
    /// creyendo que su petición se aplicó tal cual.
    pub source_note: String,
}

/// Una línea del registro, ya lista para pintar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLineView {
    /// La hora `HH:MM:SS`, en UTC.
    ///
    /// UTC y no local, igual que la columna de fecha en ISO: este árbol no
    /// lleva base de datos de husos, y una hora local inventada a partir de
    /// un desplazamiento fijo sería mentira dos veces al año. Lo que se
    /// compara aquí son líneas entre sí, y para eso el huso da igual
    /// mientras sea el mismo.
    pub time: String,
    /// El nivel, en su forma de wire — el renderer lo colorea por esto.
    ///
    /// Es una IDENTIDAD, no un texto: se compara, no se pinta. Lo que se
    /// pinta es [`Self::level_label`].
    pub level: String,
    /// El nivel tal y como se PINTA (`TRACE`), que es lo que pinta el
    /// terminal.
    ///
    /// Separado del de arriba porque son dos cosas: una identidad estable que
    /// el renderer usa para colorear y una etiqueta que se lee. Pintar la
    /// identidad es lo que tenía a la ventana enseñando `trace` en las líneas,
    /// `trace` en el chip del título y «traza» en sus botones — tres
    /// vocabularios del mismo nivel, los tres a la vez en pantalla.
    ///
    /// NO se traduce, y eso es la decisión: `TRACE` es lo que se escribe en
    /// `RUST_LOG`, lo que sale en un pegado de un informe de fallo y lo que
    /// alguien va a buscar con la vista en una lista larga. Los BOTONES de
    /// nivel de la ventana sí van traducidos: son un mando, no un dato, y el
    /// terminal no tiene ninguno con el que discrepar.
    ///
    /// `#[serde(default)]`: vacío = un puente anterior, y entonces el
    /// renderer cae a la identidad, que es lo que pintaba antes.
    #[serde(default)]
    pub level_label: String,
    /// El módulo que la emitió, enmascarado y acotado.
    pub target: String,
    /// El mensaje, enmascarado y acotado.
    ///
    /// Enmascarado como cualquier otro texto que se pinta, y aquí con un
    /// motivo propio: un mensaje de registro puede llevar dentro el nombre de
    /// un fichero que alguien eligió, y un `U+202E` ahí reordena la línea
    /// entera del panel.
    pub message: String,
    /// Lo pintado difiere de lo que hay, en el módulo o en el mensaje.
    pub hostile: bool,
    /// De qué PROCESO salió: `window` o `daemon` (#328).
    ///
    /// Por línea y no solo en la cabecera, porque en una lista mezclada es la
    /// mitad de la información: «el provider falló» y «la ventana no pudo
    /// pintarlo» se leen igual sin saber quién lo escribió, y son dos averías
    /// distintas. Cerrado y sin traducir: el renderer marca la fila, no la
    /// lee en voz alta.
    pub source: String,
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
    /// La columna de iconos está abierta en este listado (puente 62, ADR
    /// 0105): ALGUNA de sus entradas —visible o no— tiene icono, así que
    /// todas las filas llevan la celda, vacía o no, y los nombres siguen
    /// alineados. Lo decide el host desde el pane entero; un renderer que lo
    /// dedujera de las filas visibles cerraría la columna al desplazarse a
    /// una página sin iconos y correría todos los nombres.
    pub icon_column: bool,
    /// Fila bajo el cursor, si hay alguna.
    pub cursor: Option<RowKey>,
    /// Cuántas filas están marcadas en el hueco (no solo en la ventana).
    pub marks: u64,
    /// Cuántas entradas se saltó el provider, ya DICHO en el idioma del
    /// lector. Vacío = ninguna, o el provider no lleva la cuenta.
    ///
    /// Se dice en pantalla porque es la clase de fallo que no se puede
    /// descubrir mirando: lo que falta no está, y no hay ninguna fila donde
    /// el lector pueda tropezarse con ello. Un listado incompleto que se
    /// calla miente por omisión.
    ///
    /// Traducido AQUÍ, como la frase de estado de la búsqueda: un renderer
    /// no traduce, y «se saltó una» y «se saltó 3» no se dicen igual en todos
    /// los idiomas.
    pub skipped_note: String,
    /// Cuántas entradas está APARTANDO la ocultación, ya dicho en el idioma
    /// del lector. Vacío = ninguna, o la ocultación está apagada.
    ///
    /// Permanente y no un mensaje de la barra: el aviso de `pane.toggle-hidden`
    /// lo pisa la siguiente tecla, y entonces un listado que enseña menos de
    /// lo que hay se queda mudo. Misma disciplina que [`Self::skipped_note`],
    /// y traducido aquí por lo mismo — «1 oculta» y «3 ocultas» no se dicen
    /// igual en todos los idiomas.
    pub hidden_note: String,
    /// Los nombres se REINTERPRETAN con otra codificación (#57). Vacío = no.
    ///
    /// La misma disciplina que las dos de arriba, y por eso está aquí y no en
    /// la barra: lo que se pinta no son los bytes que hay en el disco, y eso
    /// hay que poder saberlo en el momento de decidir copiar o borrar algo.
    /// El mensaje del toggle se lo lleva la siguiente tecla.
    ///
    /// `#[serde(default)]` NO promete compatibilidad con un puente anterior
    /// —el renderer rechaza cualquier versión que no sea la suya—: está para
    /// que las fixtures y los round-trips no tengan que enumerar campos que
    /// casi siempre van vacíos.
    #[serde(default)]
    pub names_note: String,
    /// El listado se está RELLENANDO todavía, y cuántas van. Vacío = entero.
    #[serde(default)]
    pub filling_note: String,
    /// Marcas que el último refresco descartó porque su entrada ya no está.
    /// Vacío = no cayó ninguna.
    #[serde(default)]
    pub pruned_note: String,
    /// Cuántas entradas hay marcadas y cuánto pesan, ya dicho. Vacío = sin
    /// marcas.
    #[serde(default)]
    pub marked_note: String,
    /// Las MIGAS de la ruta (puente 65): la raíz (`⟨file⟩`, `⟨sftp⟩host`) y
    /// un tramo por directorio, cada uno ya enmascarado. Pulsar el tramo
    /// `depth` navega al directorio con esos `depth` tramos
    /// (`breadcrumb_activate`). Vacío = la ruta va entera en `path_display`.
    #[serde(default)]
    pub path_segments: Vec<String>,
    /// Cuánto del volumen está OCUPADO, en `0.0..=1.0` (puente 65): el
    /// indicador de espacio del pie. `None` = no se sabe (sin volumen, o
    /// un esquema que no lo dice).
    #[serde(default)]
    pub used_ratio: Option<f32>,
    /// El pie del listado (spec 2026-09-10): cuántos directorios y ficheros,
    /// cuánto pesan, lo marcado y el espacio libre del volumen, ya
    /// redactado. Vacío = `[ui] pane_footer` apagado.
    #[serde(default)]
    pub footer: String,
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
    ///
    /// Lleva A DÓNDE va, que es la mitad que faltaba. El cuerpo sigue
    /// enseñando el listado ANTERIOR —a propósito: si la conexión falla, el
    /// lector se queda donde estaba— y sin el destino esa mezcla no se puede
    /// leer: la pantalla enseña un sitio mientras trabaja en otro, y no dice
    /// cuál. El terminal pone el destino en la cabecera junto al spinner
    /// desde #323.
    ///
    /// El UMBRAL —nada antes de 250 ms, porque por debajo la operación
    /// termina antes de que el ojo lo registre y lo único que se ve es un
    /// parpadeo— es cosa del renderer, y es donde tiene que estar: es un
    /// retardo puramente visual, y en CSS no cuesta ni un temporizador ni un
    /// mensaje.
    Loading {
        /// La clave Fluent del VERBO, del vocabulario CERRADO compartido
        /// (`norte_frontend::busy::BusyKind`): `busy-connecting`,
        /// `busy-listing`, `busy-opening`.
        ///
        /// De ahí y no de una clave propia porque ese módulo existe desde
        /// #323 justamente para que los dos frontends no digan cosas
        /// distintas de la misma espera — y esta ventana decía «cargando…»
        /// hasta para una conexión remota, que es el caso que lo destapó.
        #[serde(default)]
        verb_key: String,
        /// La ruta a la que va, ya pintable y con la reinterpretación
        /// vigente. Vacía = un relleno del sitio en el que ya se está, que no
        /// va a ninguna parte.
        #[serde(default)]
        target_display: String,
        /// Esa ruta DIFIERE de los bytes reales.
        #[serde(default)]
        target_hostile: bool,
    },
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
// Cuatro bools, y cada uno es un hecho INDEPENDIENTE que el renderer pinta
// distinto: el nombre difiere de lo real, está bajo el cursor, está marcada,
// su insignia difiere de lo real. No es un estado que se pueda plegar — el
// lint apunta a parámetros y a máquinas de estado, no a una fila de wire.
#[expect(
    clippy::struct_excessive_bools,
    reason = "fila de wire: insignias independientes, no una máquina de estados"
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowView {
    /// Clave opaca, válida para esta generación.
    pub key: RowKey,
    /// Nombre listo para pintar.
    pub display_name: String,
    /// El nombre pintado difiere del real: bytes lossy o controles
    /// enmascarados (spec §6). El renderer DEBE marcarlo.
    pub hostile: bool,
    /// Por dónde va la tarea que trabaja sobre ESTA fila, 0–100 (puente 69,
    /// spec 2026-09-15). `None` = ninguna tarea la está tocando.
    ///
    /// Lo resuelve el host con `processes::progress_for`, que casa por ruta
    /// exacta: una copia DENTRO de un directorio no pinta el directorio a
    /// medias, porque «la mitad de esta carpeta» no es lo que el número dice.
    #[serde(default)]
    pub progress: Option<u8>,
    /// Qué es.
    pub kind: RowKind,
    /// Bajo el cursor.
    pub selected: bool,
    /// Marcada para operar.
    pub marked: bool,
    /// Celdas de las columnas configuradas, en el orden de la cabecera.
    pub cells: Vec<CellView>,
    /// La insignia que un plugin puso en esta fila, ya enmascarada y acotada.
    /// Vacía = ninguna.
    ///
    /// Cosmética por contrato (ADR 0037): un decorador que no contesta, o un
    /// catálogo caído, dejan la fila sin insignia y el listado igual.
    pub badge: String,
    /// La insignia se pinta DISTINTO de lo que es. La escribe un plugin y va
    /// pegada a un nombre de fichero.
    pub badge_hostile: bool,
    /// El rol semántico que el plugin pidió para la fila (`warning`,
    /// `error`…), del vocabulario CERRADO de `norte-theme`. Vacío = ninguno.
    ///
    /// Un nombre que no está en el vocabulario llega vacío, no crudo: el
    /// renderer lo usa para elegir un color del tema, y una cadena libre ahí
    /// sería un plugin eligiendo su propio estilo.
    pub badge_role: String,
    /// El ICONO de la fila (puente 62, ADR 0105): lo que un decorador de
    /// hueco `icon` puso, ya enmascarado y acotado. Vacío = ninguno. Se
    /// pinta a la IZQUIERDA del nombre en una columna de ancho fijo, que el
    /// renderer abre en todas las filas del hueco en cuanto una lo tiene.
    pub icon: String,
    /// El icono se pinta distinto de lo que es. Misma razón que la insignia.
    pub icon_hostile: bool,
    /// El color `#rrggbb` con que el TEMA pinta el nombre de esta entrada,
    /// por `[files.ext]` (gana) o `[files.kind]`. Vacío = el tema no dice
    /// nada de ella y el renderer usa el color normal del listado.
    ///
    /// Viaja RESUELTO y no como nombre de regla porque las extensiones son un
    /// conjunto ABIERTO: un tema colorea las que quiera, así que no hay lista
    /// de clases que el renderer pudiera conocer de antemano. Es lo contrario
    /// que [`RowView::badge_role`], que sí es vocabulario cerrado.
    #[serde(default)]
    pub name_color: String,
    /// El nombre va en NEGRITA (un directorio, un ejecutable). Del mismo
    /// estilo que `name_color`.
    #[serde(default)]
    pub name_bold: bool,
    /// Atenuado. Los presets retro atenúan así los archivos comprimidos, y
    /// sin este campo salían apagados en el terminal y a plena luz en la
    /// ventana.
    #[serde(default)]
    pub name_dim: bool,
    /// Cursiva.
    #[serde(default)]
    pub name_italic: bool,
    /// Subrayado.
    #[serde(default)]
    pub name_underline: bool,
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
    ///
    /// Una IDENTIDAD, así que viaja ENTERA o VACÍA: ni enmascarada ni
    /// recortada. Las dos cosas la rompen —enmascarar no es inyectivo y dos
    /// columnas configuradas podían colapsar en una, recortar la dejaba sin
    /// casar con la suya— y no hace falta ninguna: quien la pinta es `label`,
    /// y el renderer solo mete el id en un atributo `data-`.
    pub id: String,
    /// Etiqueta ya traducida.
    pub label: String,
    /// `asc`/`desc` si el listado se ordena por ESTA columna; `None` si no.
    pub sort: Option<String>,
    /// La columna ordena. Una que no, se pinta sin afordancia de click.
    pub sortable: bool,
    /// Ancho FIJO en celdas, si `[ui.columns] spec.width` lo fija (puente
    /// 64): la cabecera y las celdas de la columna lo siguen, y arrastrar el
    /// borde de la cabecera lo cambia. `None` = a lo que mida su contenido.
    /// Para la columna `name` es su SUELO (`columns::NAME_MIN`), no un ancho:
    /// el nombre crece, y por debajo de eso el renderer descarta columnas.
    #[serde(default)]
    pub width: Option<u16>,
    /// `left` o `right`: la alineación configurada de la columna, la misma
    /// que aplica el terminal. Solo tiene efecto con un ancho fijo.
    #[serde(default)]
    pub align: String,
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
    pub banners: Vec<BannerView>,
    /// Avisos que caducaron sin que el lector abriera el registro (spec
    /// 2026-09-10, `[ui] notice_seconds`). El renderer pinta una insignia
    /// mientras haya alguno; pulsarla abre el panel de registro por el
    /// botón de la barra de paneles. Abrirlo lo pone a cero.
    #[serde(default)]
    pub notices_unread: u32,
    /// Lo que hay tecleado a medias: una secuencia, un contador, o las dos
    /// cosas. Se pinta SIEMPRE que exista — un prefijo pendiente que no se
    /// ve es un prefijo que no se puede cancelar.
    pub pending: Option<PendingView>,
}

/// El panel de sincronización: el PLAN, antes de que nada se escriba.
///
/// Ventana como el de diferencias y por lo mismo: un plan de medio millón de
/// pasos no cruza el puente entero. Y como el de diferencias, los pasos se
/// nombran por su `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncView {
    /// La raíz ORIGEN, ya saneada y con su marca.
    pub source: DialogLine,
    /// La raíz DESTINO, ya saneada y con su marca.
    pub dest: DialogLine,
    /// El modo pedido (`update` o `mirror`), por id estable.
    ///
    /// Se pinta ANTES de aprobar y no es decoración: un `mirror` BORRA en el
    /// destino y un `update` no.
    pub mode: String,
    /// La ventana de pasos.
    pub steps: Vec<SyncStepView>,
    /// Índice del primer paso que viaja.
    pub first_visible: u64,
    /// Cuántos pasos tiene el plan, INCLUIDOS los que la lista no retiene.
    ///
    /// El modelo compartido acota cuántos cuerpos guarda y cuenta aparte los
    /// que tira; sumarlos aquí es lo que evita que este número y el de la
    /// línea de estado se contradigan en un plan grande.
    pub total: u64,
    /// El RESUMEN del plan, ya dicho: cuántos irreversibles, cuántos bytes,
    /// qué no se pudo leer, y si la lista esconde pasos.
    ///
    /// Es lo que un humano necesita antes de aprobar, y no cabe en la línea
    /// de estado: un plan aprobable con tres pasos irreversibles y una rama
    /// ilegible se leía como «5 pasos, pulsa aprobar».
    pub summary: Vec<String>,
    /// Lo que IMPIDE sincronizar, ya dicho, CON su ruta. Vacío = nada lo
    /// impide.
    pub blockers: Vec<SyncBlockerView>,
    /// Cuántos bloqueos hay DE VERDAD.
    ///
    /// El wire recorta la lista, y el total viaja aparte a propósito: un
    /// humano necesita saber que hay cuarenta mil aunque solo se le enseñen
    /// doscientos cincuenta y seis.
    pub blockers_total: u64,
    /// El estado, ya dicho: planificando, listo para aprobar, aplicando…
    pub status: String,
    /// Qué se puede hacer ahora, ya dicho (la línea de ayuda del pie).
    pub hint: String,
    /// La SEGUNDA pregunta, ya formulada, cuando el plan es peligroso.
    ///
    /// `None` = todavía no se ha pedido aprobar, o este plan no la necesita
    /// (todo se puede deshacer y no borra árboles). La compone el modelo
    /// compartido, con una rama por perspectiva de deshacer: un titular que
    /// diga «algo de esto se puede deshacer» sobre una confirmación que diga
    /// «nada» enseña a saltarse las dos.
    pub confirming: Option<String>,
    /// Los pasos que FALLARON, cuando la sincronización terminó.
    ///
    /// El recuento va en la línea de estado; esto es el detalle: qué ruta y
    /// por qué. El desenlace de la Task dice si corrió, y lo que no se hizo
    /// lo cuenta solo el informe.
    pub failures: Vec<SyncFailureView>,
    /// Ya se pidió PARAR lo que está corriendo.
    ///
    /// Viaja porque si no, pulsar `Escape` durante la escritura no cambia ni
    /// una letra de la pantalla: no hay forma de distinguir «te oí» de «esta
    /// tecla no hace nada», que es justo lo que empuja a pulsarla otra vez.
    pub cancel_requested: bool,
    /// El plan se puede aprobar YA.
    ///
    /// Lo decide el modelo compartido: un plan sin cerrar, con bloqueos, o ya
    /// enviado, no se aprueba — y que el pie ofrezca aprobar lo que el modelo
    /// va a rechazar es la pantalla rota que esto evita.
    pub can_approve: bool,
    /// Hay una Task corriendo (la del plan, o la de la aplicación).
    pub running: bool,
}

/// Un paso que falló al aplicar el plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncFailureView {
    /// Por qué falló, ya traducido.
    pub cause: String,
    /// Sobre qué ruta, ya saneada.
    pub path: String,
    /// Lo pintado difiere de los bytes.
    pub path_hostile: bool,
    /// De qué raíz cuelga la ruta (`source`, `dest` o `either`), por id
    /// estable — para el estilo, no para leer.
    pub anchor: String,
    /// Lo mismo, ya traducido y para PINTAR. Vacío = no hay nada que decir.
    ///
    /// Viaja además del id porque el id no se lee: en un panel donde una ruta
    /// sin calificar significa «del origen», callar un `either` es afirmar el
    /// origen, y un atributo `data-` que ningún estilo mira lo calla igual.
    pub anchor_label: String,
}

/// Algo que impide sincronizar, con dónde pasa.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncBlockerView {
    /// Qué es, ya traducido.
    pub label: String,
    /// Sobre qué ruta, ya saneada. La RAÍZ se dice «todo el árbol» y no como
    /// una cadena vacía.
    pub path: String,
    /// Lo pintado difiere de los bytes.
    pub path_hostile: bool,
}

/// Un paso del plan, ya listo para pintar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStepView {
    /// Su id dentro del plan: la identidad, jamás la posición.
    pub id: u64,
    /// Qué hace, ya traducido.
    pub kind: String,
    /// Por qué, ya traducido.
    pub reason: String,
    /// Si el deshacer lo devuelve, ya dicho.
    ///
    /// Nunca sale de `reversal` a secas: esa es la mitad de la respuesta, y
    /// la que miente cuando el destino no tiene papelera.
    pub undo: String,
    /// De qué raíz cuelga la ruta (`source` o `dest`), por id estable.
    pub anchor: String,
    /// Lo mismo, ya traducido y para pintar. Vacío = no hay nada que decir.
    pub anchor_label: String,
    /// La ruta relativa, enmascarada.
    pub path: String,
    /// Lo pintado difiere de los bytes.
    pub path_hostile: bool,
    /// La ortografía del DESTINO, cuando sus bytes difieren de la del origen.
    ///
    /// La escritura cae sobre ESTA. Campo propio y no un sufijo del nombre:
    /// dos ortografías en la misma celda las puede juntar un nombre.
    pub dest_path: Option<String>,
    /// Lo pintado del destino difiere de sus bytes.
    pub dest_path_hostile: bool,
    /// Las dos ortografías se rinden IGUAL (un par NFC/NFD), así que el
    /// lector no puede verlas distintas y hay que decírselo.
    pub twins: bool,
}

/// El panel de diferencias: dos árboles comparados, fila a fila.
///
/// Ventana y no lista entera, por el mismo motivo que un listado: el motor
/// emite una fila por nombre emparejado de TODO el árbol y nada lo acota —un
/// tope convertiría «¿son iguales?» en una respuesta a medias—, así que medio
/// millón de filas no pueden cruzar el puente. Viaja lo que se ve, con su
/// primera fila y el total.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareView {
    /// La raíz izquierda, ya saneada (el panel que lanzó la comparación).
    pub left: String,
    /// Lo pintado a la izquierda difiere de la ruta real.
    pub left_hostile: bool,
    /// La raíz derecha, ya saneada.
    pub right: String,
    /// Lo pintado a la derecha difiere de la ruta real.
    pub right_hostile: bool,
    /// La ventana de filas VISIBLES (las que un filtro no esconde).
    pub rows: Vec<CompareRowView>,
    /// Índice, dentro de las visibles, de la primera fila que viaja.
    pub first_visible: u64,
    /// Cuántas filas visibles hay en total.
    pub total: u64,
    /// La fila seleccionada, por su id. Anclada al id y no al índice: un
    /// filtro esconde filas, jamás las renumera.
    pub selected: Option<u64>,
    /// Los filtros por categoría, en orden fijo.
    pub filters: Vec<CompareFilterView>,
    /// El estado, ya dicho: cuántas van y si sigue caminando.
    pub status: String,
    /// La comparación sigue corriendo.
    pub running: bool,
}

/// Un filtro por categoría, con su recuento.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareFilterView {
    /// Id estable de la categoría (`same`, `different`…), para el renderer.
    pub id: String,
    /// Cómo se llama, en el idioma del lector.
    pub label: String,
    /// Cuántas filas cayeron en ella, filtros aparte.
    pub count: u64,
    /// Está ESCONDIENDO su categoría.
    pub hidden: bool,
}

/// Una fila del panel de diferencias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareRowView {
    /// Su id dentro de esta comparación. Es la IDENTIDAD: seleccionar y
    /// marcar van por él, jamás por la posición.
    pub id: u64,
    /// El veredicto, ya traducido.
    pub verdict: String,
    /// Su categoría, por id estable (para pintar el color).
    pub category: String,
    /// Cuánto vale el veredicto, ya traducido.
    pub confidence: String,
    /// Qué rung lo decidió, ya traducido.
    pub criterion: String,
    /// El porqué, ya traducido, cuando el veredicto tiene porqué.
    pub reason: Option<String>,
    /// La cara izquierda, ausente en un huérfano de la derecha.
    pub left: Option<CompareFaceView>,
    /// La cara derecha.
    pub right: Option<CompareFaceView>,
    /// Por qué esta fila enseña DOS ortografías, en una frase ya traducida.
    ///
    /// Frase y no insignia pegada al nombre, y eso no es estilo: lo que se
    /// pega a un nombre lo puede falsificar un nombre.
    pub paired_under: Option<String>,
}

/// Una cara de una fila: lo que se sabe de una entrada, ya saneado.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareFaceView {
    /// El nombre, ya enmascarado.
    pub name: String,
    /// Lo pintado difiere de los bytes que hay.
    pub hostile: bool,
    /// El tamaño ya formateado, o vacío si el provider no lo sabe.
    ///
    /// Vacío y no un `0` fabricado: «no lo sé» y «cero bytes» son dos
    /// respuestas distintas, y un huérfano sin hidratar da la primera.
    pub size: String,
    /// La fecha ya formateada, o vacía si no se sabe.
    pub mtime: String,
    /// Es un directorio.
    pub is_dir: bool,
}

/// Un aviso persistente de la barra.
///
/// No es una cadena pelada, y los dos campos que la acompañan son por lo
/// mismo que en un diálogo. La marca: el aviso de una sesión en claro pinta
/// un `host` que viene del WIRE, se enmascara, y sin bandera la ausencia de
/// insignia se lee como «esto es fiel» — en el indicador donde más valor
/// tiene para quien ataca. Y el sujeto aparte: montar `{scheme}://{host}`
/// dentro de la frase convierte a `bank.example@evil.example` en algo que se
/// lee como userinfo de un host legítimo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BannerView {
    /// La frase, ya traducida y sin nada que venga de fuera dentro.
    pub text: String,
    /// De qué conexión habla, si habla de una.
    pub subject: Option<BannerSubjectView>,
}

/// La conexión de la que habla un aviso: cada parte en su campo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BannerSubjectView {
    /// Esquema, ya enmascarado y acotado.
    pub scheme: String,
    /// Host, ya enmascarado y acotado.
    pub host: String,
    /// POR QUÉ está degradada, ya traducido (#279).
    ///
    /// Un motivo que este binario no conoce dice «motivo desconocido» y no
    /// hereda la frase del que sí conoce: un aviso de seguridad no puede
    /// afirmar una causa que nadie ha dicho.
    pub reason: String,
    /// El detalle humano del wire, enmascarado y acotado, y solo cuando el
    /// motivo es desconocido — que es cuando el contrato del proto dice
    /// apoyarse en él.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Lo pintado difiere de lo que hay (en el esquema, el host o el detalle).
    pub hostile: bool,
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
    /// A DÓNDE va lo que este diálogo pregunta, si va a alguna parte.
    ///
    /// Campo propio y no la primera línea del cuerpo, y eso NO es estilo. Un
    /// cuerpo plano solo puede distinguir «el destino» de «los orígenes» con
    /// un separador dentro del texto —una flecha, dos puntos—, y un nombre de
    /// directorio puede contener ese separador: `→` (U+2192) es legítimo, no
    /// es un peligro de terminal y por tanto no se enmascara ni se marca. Un
    /// directorio llamado `docs → /casa/BORRAR` produciría una línea que se
    /// lee como dos rutas, y quien confirma un movimiento cree que sus
    /// ficheros van a la segunda. La fixture `arrow_join_spoof` del corpus
    /// canónico dice exactamente esto: etiquetar FUERA DE BANDA, jamás por
    /// un separador dentro del texto.
    pub destination: Option<DialogLine>,
    /// QUÉ se pregunta, cuando eso es una cosa nombrable aparte de las rutas
    /// (la op de un agente: `delete`, `copy`…).
    ///
    /// Campo propio por el mismo motivo que [`Self::destination`]: mezclado
    /// con las rutas era una línea más, indistinguible de un nombre de
    /// fichero que dijera lo mismo.
    pub subject: Option<DialogLine>,
    /// QUIÉN pregunta, si no es quien está delante: la sesión del agente que
    /// pidió la operación.
    ///
    /// Se descartaba, y era lo primero que hay que saber para decidir: el
    /// título dice «aprobación de agente» y sin esto no se sabe de QUÉ
    /// agente.
    pub asker: Option<DialogLine>,
    /// Cuándo deja de aceptarse la respuesta, ya traducido. `None` = no hay
    /// plazo, o no se conoce.
    ///
    /// Fuera del cuerpo, otra vez por lo mismo: entre líneas de rutas, un
    /// fichero llamado `caduca en 3600 s` es la única línea con pinta de
    /// plazo cuando el plazo REAL no se conoce —una pendiente reconstruida
    /// por el resync de `policy.pending` no transporta el TTL restante—.
    pub deadline: Option<String>,
    /// CUÁNDO vence, en epoch-ms, para que el renderer pueda contar (#279).
    ///
    /// [`Self::deadline`] es una frase calculada al ABRIR, así que se congela:
    /// un modal que lleva cuatro minutos delante seguía diciendo «caduca en
    /// 300 s». No miente de forma peligrosa —el diálogo se cierra solo al
    /// vencer— pero deja de informar justo cuando más falta hace.
    ///
    /// Viaja el instante y no los segundos restantes porque lo que se necesita
    /// es una referencia FIJA: los segundos habría que refrescarlos con otro
    /// parche por segundo, que es exactamente el trabajo que esto evita.
    /// Host y renderer comparten máquina, así que comparten reloj.
    ///
    /// `None` = no hay plazo o no se conoce (una pendiente reconstruida por el
    /// resync de `policy.pending` no transporta el TTL restante). Entonces el
    /// renderer pinta la frase tal cual y no cuenta nada: contar hacia atrás
    /// desde un plazo inventado sería peor que no contar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at_ms: Option<i64>,
    /// Líneas de cuerpo, ya saneadas y acotadas.
    ///
    /// Cuando son RUTAS, el renderer las numera por posición: la etiqueta es
    /// estructural y un nombre de fichero no puede escribirla.
    pub body: Vec<DialogLine>,
    /// El cuerpo enseña MENOS elementos de los que la operación toca, y esto
    /// lo dice ya traducido. Vacío = los enseña todos.
    ///
    /// El cuerpo se acota (una selección de mil ficheros no cabe en un
    /// diálogo), y una lista recortada sin decirlo describe una operación más
    /// pequeña que la que se va a ejecutar: alguien marca doscientos, ve
    /// dieciséis y confirma. Es el único sitio donde todavía se puede decir
    /// que no.
    ///
    /// Traducido AQUÍ y en su propio campo, por los dos motivos de siempre:
    /// un renderer no traduce ni sustituye números, y un aviso metido entre
    /// las líneas del cuerpo lo podría suplantar un nombre de fichero.
    pub overflow_note: String,
    /// Alguna de las que NO se enseñan se pintaría alterada.
    ///
    /// El badge de hostil solo puede hablar de lo que se puede mirar, y lo
    /// recortado no está aquí para inspeccionarlo — pero que ahí fuera haya
    /// algo con bidi o invisibles sí se puede decir, y es lo que decide si
    /// merece la pena ampliar antes de aprobar. El terminal lo dice desde
    /// siempre en su resumen; esta ventana no, y eran las mismas rutas.
    ///
    /// `#[serde(default)]`: ausente = `false`, que es no marcar. La dirección
    /// segura es la contraria a la del badge de una ruta VISIBLE —allí callar
    /// esconde algo que se está mirando— porque aquí un badge de más sobre un
    /// recorte enseña a ignorarlo.
    #[serde(default)]
    pub overflow_hostile: bool,
    /// En qué punto está la comprobación del DESTINO: si cabe (#149) y si
    /// sabe sujetar lo que se escriba en él (#164).
    ///
    /// `#[serde(default)]`: un renderer de un puente anterior no lo manda, y
    /// su ausencia es [`DestCheckView::NotAsked`], que es lo que era antes.
    #[serde(default)]
    pub dest_check: DestCheckView,
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
    /// El campo es una CONTRASEÑA (#327).
    ///
    /// Cuando es `true`, [`Self::input`] lleva PUNTOS —uno por carácter— y no
    /// el texto: lo tecleado se queda en el host, en un buffer que se pisa con
    /// ceros al soltarlo (`norte_frontend::secret::TypedSecret`). El renderer
    /// pinta el campo como contraseña y **nunca lo resiembra** con este valor,
    /// que convertiría lo que el usuario escribió en una fila de puntos
    /// literales.
    ///
    /// Un campo propio y no «adivínalo por el título» porque esta es la única
    /// diferencia que importa entre pintar un nombre de fichero y pintar una
    /// contraseña, y dejarla implícita significa que el siguiente diálogo que
    /// pida un secreto la herede mal.
    ///
    /// `#[serde(default)]`: un renderer de un puente anterior no lo manda, y
    /// su ausencia significa «no es un secreto», que es lo que era antes.
    #[serde(default)]
    pub input_secret: bool,
}

/// Qué se sabe de A DÓNDE VAN LOS BYTES, mientras se pregunta.
///
/// De una transferencia es el directorio destino; de un borrado es la
/// papelera, o su ausencia — que es el mismo tipo de hecho y por eso comparte
/// canal: «⚠ SIN papelera: esto no se puede deshacer» responde a la misma
/// pregunta que «no cabe» y «este destino no confina». Un canal y no tres
/// también porque el renderer los pinta en un bloque que un nombre de fichero
/// no puede suplantar, y tres bloques serían tres sitios donde olvidarse de
/// esa propiedad.
///
/// **Tres estados y no una lista de avisos, porque el silencio tenía que
/// significar una sola cosa.** Las dos preguntas —¿cabe?, ¿sabe confinar?—
/// son I/O, así que el diálogo se pinta antes de que vuelvan; con un solo
/// `Vec` vacío, «todavía no lo he preguntado» y «lo pregunté y no hay nada
/// que decir» llegaban idénticos, y el humano puede confirmar en ese hueco.
/// La ausencia de la línea de #164 SIGNIFICA «este destino sujeta sus
/// escrituras», así que dejarla ambigua es afirmarlo sin saberlo.
///
/// El terminal no tiene este problema: pregunta en la cabecera de la vuelta,
/// antes de pintar, así que su modal nunca se ve sin las respuestas puestas
/// (`norte_tui::turn`). Esperarlas aquí dejaría F5 sin pintar nada contra un
/// SFTP lento, que es peor: lo que el humano tiene delante mientras tanto es
/// la lista de lo que va a copiar, que es lo que vino a leer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum DestCheckView {
    /// Este diálogo no tiene nada que comprobar sobre a dónde van los bytes,
    /// y es el valor por defecto. Los que sí: transferir, soltar y borrar.
    #[default]
    NotAsked,
    /// Se preguntó y no ha vuelto. El renderer lo DICE y reserva el sitio:
    /// una línea que aparece de golpe encima de los botones los mueve bajo
    /// el puntero de quien iba a pulsar.
    Checking,
    /// Volvió. La lista vacía es la respuesta normal y no se pinta: que
    /// quepa y que confine NO se anuncian, porque una línea en cada copia es
    /// ruido y el ruido enseña a saltarse la línea el día que dice algo.
    ///
    /// Ya traducidas y sin una sola cadena que controle un tercero. Van
    /// aquí y no entre las líneas del cuerpo por eso mismo: ahí un nombre de
    /// fichero las podría suplantar.
    Done {
        /// Lo que hay que saber antes de decir que sí. Vacío = nada.
        warnings: Vec<String>,
    },
}

/// Una línea del cuerpo de un diálogo.
///
/// Estructura y no una cadena suelta porque la línea lleva DOS cosas: lo que
/// se pinta y si lo que se pinta difiere de lo que hay. Un cuerpo de
/// `Vec<String>` con un `Vec<bool>` al lado son dos vectores que se pueden
/// desincronizar; una fila del listado ([`RowView`]) ya resuelve lo mismo
/// así, y esto es lo mismo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogLine {
    /// El texto, enmascarado y acotado.
    pub text: String,
    /// Lo pintado DIFIERE de lo real (bytes no UTF-8, controles, marcas de
    /// dirección). El renderer lo marca; jamás lo esconde.
    ///
    /// Aquí importa más que en ningún otro sitio: el cuerpo de un diálogo es
    /// lo que alguien lee antes de aprobar que se borre, se copie o se mueva
    /// un fichero. Un nombre que se pinta distinto de lo que es, sin insignia,
    /// es un nombre que se lee como fiel — y la aprobación es de OTRA cosa.
    pub hostile: bool,
}

/// El plan de renombrado que un modelo propuso, en revisión.
///
/// Una pantalla propia y no un diálogo, por lo que TIENE que enseñar: parejas
/// que se recorren, un veredicto del core que llega DESPUÉS de abrirse, y un
/// detalle de colisiones. Un diálogo es una pregunta con respuestas; esto es
/// un documento que se lee antes de aprobarlo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRenameView {
    /// El directorio sobre el que se planeó.
    pub dir: DialogLine,
    /// La ventana de parejas que viaja, NO el plan entero.
    pub pairs: Vec<AiRenamePairView>,
    /// La primera pareja de `pairs` dentro del plan.
    pub first_visible: u64,
    /// Cuántas parejas tiene el plan.
    pub total: u64,
    /// Cuánto se ve de cuánto hay, ya traducido. Vacío = se ve todo.
    ///
    /// Traducido AQUÍ y no en el renderer, y esta vez con una razón medida:
    /// el catálogo que cruza el puente lleva las cadenas YA formateadas y sin
    /// argumentos, y Fluent escribe una variable ausente como `{$shown}` —
    /// sin espacios. El renderer sustituía `{ $shown }`, que no casa nunca,
    /// así que la línea que dice cuánto del plan se está mirando pintaba dos
    /// identificadores crudos en la pantalla donde se aprueba un lote.
    pub more_note: String,
    /// FUERA de la ventana hay algún nombre que se pinta distinto de lo que
    /// es.
    ///
    /// La ventana son cinco parejas de hasta 256, y cada línea visible lleva
    /// su marca. Sin esto, la marca solo existe para lo que se ve: basta con
    /// poner la pareja alterada en la posición doce para que se apruebe un
    /// plan sin que ninguna insignia haya aparecido jamás.
    pub hidden_hostile: bool,
    /// El veredicto del core, ya traducido: comprobando, aplicable, no
    /// aplicable, o no comprobado. Es la línea que no se puede perder.
    pub status: String,
    /// La maquinaria del planificador y las colisiones, una por línea y ya
    /// traducidas, cada una diciendo si lo pintado difiere de lo real.
    pub detail: Vec<DialogLine>,
    /// Aprobar puede hacer algo. Lo dice el CORE (`executable`), no una
    /// cuenta de colisiones: el campo es normativo y un veredicto futuro
    /// puede parar un plan sin nombre ofensor que listar.
    pub confirmable: bool,
    /// Cuántos renombrados hará DE VERDAD, ya dicho y traducido. No es
    /// `total`: el planificador tira las parejas nulas, y prometer las
    /// pedidas sería prometer de más.
    ///
    /// Vacío mientras no haya veredicto: hasta que el core conteste no se
    /// sabe, y un cero se leería como «no hará nada».
    pub real_steps_note: String,
    /// El lector ha recorrido el plan ENTERO.
    ///
    /// Aprobar lo exige. Con 256 parejas permitidas y cinco visibles, la
    /// pareja doscientos se ejecutaba sin que nadie la hubiera pintado nunca
    /// — y la revisión es toda la defensa que hay contra un plan que un
    /// modelo escribió a partir de nombres que un atacante controla.
    pub seen_all: bool,
}

/// El plan de ORGANIZAR en revisión (fase 8).
///
/// Es el gemelo de [`AiRenameView`] con dos diferencias, y las dos vienen de
/// lo mismo: aquí lo que cambia es la FORMA del directorio.
///
/// - El cuerpo es un ÁRBOL, no una lista de parejas. Una lista de cuarenta
///   `a.pdf → facturas/2026/a.pdf` no deja ver cuántas carpetas aparecen ni
///   qué acaba dentro de cada una, que es justo lo que se está aprobando.
/// - No hay veredicto que esperar. El token del plan viaja CON él, así que
///   esta pantalla nace aprobable y no pasa por un `Pending`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizeView {
    /// El directorio sobre el que se planeó.
    pub dir: DialogLine,
    /// La ventana de líneas del árbol que viaja, NO el árbol entero.
    pub lines: Vec<OrganizeLineView>,
    /// La primera línea de `lines` dentro del árbol.
    pub first_visible: u64,
    /// Cuántas líneas tiene el árbol.
    pub total: u64,
    /// Cuánto se ve de cuánto hay, ya traducido. Vacío = se ve todo.
    pub more_note: String,
    /// FUERA de la ventana hay algún nombre que se pinta distinto de lo que
    /// es. Sin esto, la marca solo existe para lo que se ve.
    pub hidden_hostile: bool,
    /// «Crea N carpetas y mueve M ficheros», ya traducido: lo que se lee para
    /// decidir sin contar líneas. Va ANTES del árbol.
    pub summary: String,
    /// El lector ha recorrido el árbol ENTERO. Aprobar lo exige.
    pub seen_all: bool,
}

/// Una línea del árbol de organizar (fase 8).
///
/// El `kind` viaja como DATO y no resuelto a un color: el renderer decide
/// cómo se ve una carpeta nueva, y un tema monocromo necesita poder marcarla
/// de otra forma. Que sea nueva o no lo decide
/// [`norte_frontend::organize::tree_lines`], compartido con el terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizeLineView {
    /// Cuánto se sangra: 0 es hijo directo del directorio del plan.
    pub depth: u32,
    /// El nombre, enmascarado y acotado, con su marca si difiere.
    pub text: DialogLine,
    /// Qué es: una carpeta que se CREA, una que ya estaba, o un fichero que
    /// se mueve.
    pub kind: OrganizeLineKind,
}

/// Qué representa una línea del árbol de organizar (fase 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrganizeLineKind {
    /// Una carpeta que el plan va a CREAR.
    NewDir,
    /// Una carpeta que YA existe y a la que el plan mete algo.
    ExistingDir,
    /// Un fichero que se mueve hasta ahí.
    Moved,
}

/// Una pareja del plan: de qué nombre a qué nombre.
///
/// Los dos nombres van ENTEROS y por separado, jamás concatenados con una
/// flecha: el mismo motivo que el destino de una transferencia
/// ([`DialogView::destination`]) — un nombre puede contener la flecha.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRenamePairView {
    /// El nombre de ahora.
    pub from: DialogLine,
    /// El que propone el modelo.
    pub to: DialogLine,
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

/// La pantalla de arranque (puente 69, ADR 0115).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplashView {
    /// El arte, una línea por fila. Viene del modelo compartido, así que la
    /// brújula es la misma que pinta el terminal.
    pub art: Vec<String>,
    /// Qué build corre.
    pub version: String,
    /// Y con qué revisión se compiló.
    pub revision: String,
    /// Contra qué core habla, ya traducido.
    pub daemon: String,
    /// El pie: cómo se quita, y si los números hacen algo.
    pub hint: String,
    /// Las secciones, ya filtradas: ninguna viene vacía.
    pub sections: Vec<SplashSectionView>,
    /// Cuánto le queda puesta, en milisegundos, o `None` si se queda hasta
    /// que alguien la quite.
    ///
    /// El plazo lo decide el host —es suyo el modo `brief` y suyo el reloj—,
    /// pero quien lo cumple es el renderer: aquí no hay bucle de eventos que
    /// despierte solo, como sí lo hay en el terminal, y una pantalla que se
    /// promete breve y se queda puesta hasta que tocas una tecla es peor que
    /// no prometer nada. Así que el número CRUZA, en vez de que el renderer
    /// se invente el suyo: dos relojes con la misma constante escrita dos
    /// veces es exactamente la divergencia que el ADR 0077 persigue.
    pub close_after_ms: Option<u32>,
}

/// Una sección de la pantalla de arranque (puente 69).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplashSectionView {
    /// El título, ya traducido y saneado.
    pub title: String,
    /// Sus filas, en el orden en que se pintan.
    pub rows: Vec<SplashRowView>,
}

/// Una fila de la pantalla de arranque (puente 69).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplashRowView {
    /// El número que la abre, o `0` si la fila no tiene tecla.
    pub number: u8,
    /// Lo que se lee, ya saneado.
    pub label: String,
    /// El detalle a la derecha (una ruta, un número de visitas).
    pub detail: String,
}

/// Una task del tablero.
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
    /// El ritmo, ya escrito (`1.2 MiB/s`), o vacío si no se sabe. Puente 69.
    ///
    /// Escrito por el HOST y no un número: `human_rate` es del crate
    /// compartido, así que el terminal y la ventana dicen la misma velocidad
    /// con las mismas unidades, y el renderer no elige redondeos.
    #[serde(default)]
    pub rate: String,
    /// Lo que queda, ya escrito (`1m 20s`), o vacío. Puente 69.
    #[serde(default)]
    pub eta: String,
    /// Descripción corta ya saneada (qué se está moviendo).
    pub detail: Option<String>,
    /// [`Self::detail`] difiere de la ruta real. Se marca por el mismo motivo
    /// que en [`DialogLine::hostile`]: una copia cuyo fichero en curso se
    /// pinta con el nombre enmascarado y sin insignia dice que ese ES el
    /// nombre.
    pub detail_hostile: bool,
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
        /// La columna de iconos está abierta (puente 62): va CON las filas
        /// porque es con un parche de filas como aterrizan los iconos, y un
        /// renderer que se quedara con el valor de la última foto pintaría
        /// la primera página de iconos sin su columna.
        icon_column: bool,
        /// Cuántas filas tiene el listado ENTERO, no cuántas viajan.
        ///
        /// Viaja en el parche y no solo en la foto porque es la ALTURA del
        /// desplazamiento del renderer (`total * alto_de_celda`, más
        /// `aria-rowcount`), y el drenaje paginado contesta con parches
        /// —también el último lote—. Sin esto el renderer se quedaba con el
        /// total de la primera página para siempre: un directorio de cinco
        /// mil ficheros topaba en la fila 100, y ni la rueda podía bajar ni
        /// el rango visible podía pedir el resto.
        total_rows: Option<u64>,
    },
    /// La CABECERA de un listado cambió: su ruta y lo que falta de él.
    ///
    /// Aparte de [`ViewChange::Rows`] porque es otra parte de la pantalla —el
    /// renderer la pinta en `paintHeader`—, y con ella porque se mueven a la
    /// vez: `pane.names-encoding` retranscribe la ruta igual que las filas, y
    /// `pane.toggle-hidden` mueve entradas dentro y fuera del listado, lo que
    /// cambia cuántas se apartan.
    ///
    /// Antes solo viajaba en la foto entera, así que las filas se repintaban
    /// y el título se quedaba con la lectura vieja — el mojibake arriba y el
    /// lector sin saber si el comando hizo algo (#57, #293).
    BrowserHeader {
        /// Hueco.
        slot_id: u32,
        /// La ruta, ya pintable y con la reinterpretación vigente.
        path_display: String,
        /// Esa ruta DIFIERE de los bytes reales.
        path_hostile: bool,
        /// Lo que el provider se saltó, ya dicho. Vacío si no se saltó nada.
        skipped_note: String,
        /// Lo que la ocultación aparta. Vacío si no aparta nada.
        hidden_note: String,
        /// Los nombres se REINTERPRETAN con otra codificación (#57). Vacío si
        /// no.
        ///
        /// Permanente mientras dure, como en el terminal: lo que se pinta no
        /// son los bytes que hay en el disco, y eso hay que poder saberlo en
        /// el momento de decidir copiar o borrar algo — no solo en el mensaje
        /// del toggle, que la siguiente tecla se lleva.
        names_note: String,
        /// El listado se está RELLENANDO todavía, y cuántas van. Vacío si ya
        /// está entero.
        ///
        /// Un listado incompleto jamás es silencioso: sin esto la pantalla
        /// afirma que eso es todo lo que hay, que es precisamente lo que
        /// todavía no se sabe.
        filling_note: String,
        /// Marcas que el último refresco descartó porque su entrada ya no
        /// está. Vacío si no cayó ninguna.
        ///
        /// El más grave de los avisos de esta cabecera: con la selección
        /// vacía el embudo del operando cae al CURSOR, así que callarlo
        /// redirige la siguiente operación en masa a algo que nadie marcó.
        pruned_note: String,
        /// Cuántas entradas hay marcadas y cuánto pesan, ya dicho. Vacío sin
        /// marcas: quien no marca no gana ruido.
        marked_note: String,
        /// El pie del listado (spec 2026-09-10), ya redactado; viaja con la
        /// cabecera porque cambia con lo mismo que ella: marcar, ocultar,
        /// rellenar. Vacío = `[ui] pane_footer` apagado.
        #[serde(default)]
        footer: String,
        /// Las migas de la ruta (puente 65); ver `BrowserSlotView`.
        #[serde(default)]
        path_segments: Vec<String>,
        /// Cuánto del volumen está ocupado (puente 65); ver `BrowserSlotView`.
        #[serde(default)]
        used_ratio: Option<f32>,
        /// Cuántas entradas hay marcadas, en crudo.
        ///
        /// Sigue viajando al lado de [`Self::BrowserHeader::marked_note`] y
        /// no es una duplicación: la frase es para PINTAR y este número es
        /// para decidir (un renderer que quiera marcar el hueco, contar, o
        /// habilitar algo), y derivar un número de una frase traducida es lo
        /// que este DTO existe para no obligar a nadie a hacer.
        marks: u64,
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
        /// Qué fila del panel de procesos está elegida, sobre estas filas.
        ///
        /// Viaja CON el tablero y no en un `ViewChange` propio, por lo mismo
        /// que `total_rows` viaja con las filas de un listado: es la extensión
        /// de lo que va al lado, y las dos se mueven a la vez. Una tarea que
        /// caduca a los diez segundos quita una fila y desplaza el resto; sin
        /// esto, el cursor solo viajaba en la foto entera, así que el panel
        /// seguía resaltando la fila N —que ya es otra tarea, o ninguna—
        /// mientras la tecla de cancelar actuaba sobre la que el host tiene
        /// acotada. Resaltar una y parar otra es la avería, no el retraso.
        ///
        /// `None` con el tablero vacío: un índice sin fila detrás resalta la
        /// nada.
        #[serde(default)]
        cursor: Option<u64>,
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
    /// El selector de perfiles se abrió, se movió o se cerró.
    Profiles {
        /// El selector, o `None` si se cerró.
        profiles: Option<ProfilePickerView>,
    },
    /// La barra de menús: se desplegó uno, se movió el cursor, o se cerró.
    ///
    /// Entero y no un delta, como la ayuda: la barra y el desplegable son un
    /// todo pequeño, y mandarlo por trozos sería inventarse un protocolo para
    /// ahorrar unos cientos de bytes.
    Menu {
        /// La barra, siempre: la fila de títulos sigue ahí con el
        /// desplegable cerrado.
        menu: MenuView,
    },
    /// La barra de paneles cambió: se abrió o cerró un panel, se movió el
    /// teclado, o algo empezó a tener algo que contar.
    ///
    /// No la emite ningún sitio en particular: el host la compara con la
    /// última que mandó cada vez que arma un parche, y la añade si difiere.
    /// Es lo que hace que un panel abierto por tecla, por menú, por paleta o
    /// por la propia barra la actualice igual — el «por frame» de la TUI,
    /// traducido a un puente que solo habla cuando algo cambia.
    PanelBar {
        /// La barra entera.
        panel_bar: PanelBarView,
    },
    /// La barra de teclas cambió: otra pantalla tiene el teclado, o un
    /// perfil trajo otro keymap. Mismo mecanismo que la de paneles: el host
    /// la compara con la última que mandó al armar cada parche.
    KeyBar {
        /// La barra entera.
        key_bar: KeyBarView,
    },
    /// La paleta se abrió, se filtró, se movió o se cerró.
    Palette {
        /// La paleta, o `None` si se cerró.
        palette: Option<PaletteView>,
    },
    /// La pantalla de arranque se puso o se quitó (puente 69).
    Splash {
        /// La pantalla, o `None` si se quitó.
        splash: Option<SplashView>,
    },
    /// El asistente de primer arranque se abrió, se movió o se cerró.
    Wizard {
        /// El asistente, o `None` si se cerró.
        wizard: Option<WizardView>,
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
    /// El tema se abrió o se cerró.
    Theme {
        /// El tema, o `None` si se cerró.
        theme: Option<ThemeView>,
    },
    /// La búsqueda arrancó, encontró algo, terminó o se cerró.
    Search {
        /// La búsqueda, o `None` si se cerró.
        search: Option<SearchView>,
    },
    /// El panel de diferencias cambió (se abrió, llegaron filas, se cerró).
    Compare {
        /// La comparación, o `None` si se cerró.
        compare: Option<CompareView>,
    },
    /// El panel de sincronización cambió (se abrió, llegaron pasos, se cerró).
    Sync {
        /// El plan, o `None` si se cerró.
        sync: Option<SyncView>,
    },
    /// El selector de disposiciones se abrió, se movió o se cerró.
    Layouts {
        /// El selector, o `None` si se cerró.
        layouts: Option<LayoutPickerView>,
    },
    /// El selector de columnas se abrió, se movió o se cerró.
    ColumnsPicker {
        /// El selector, o `None` si se cerró.
        columns: Option<ColumnsPickerView>,
    },
    /// Un selector se abrió, se movió o se cerró.
    Picker {
        /// El selector, o `None` si se cerró.
        picker: Option<PickerView>,
    },
    /// Las extensiones se abrieron, cambiaron o se cerraron.
    Extensions {
        /// El gestor, o `None` si se cerró.
        extensions: Option<ExtensionsView>,
    },
    /// El panel de sesiones de agente se abrió, se movió o se cerró.
    Agents {
        /// El panel, o `None` si se cerró.
        agents: Option<AgentsView>,
    },
    /// La salida de un comando de extensión se enseñó o se cerró.
    PluginOutput {
        /// Lo que imprimió, o `None` si se cerró.
        output: Option<ExtensionOutputView>,
    },
    /// La salida de un programa (#312) se enseñó o se cerró.
    ProgramOutput {
        /// Lo que imprimió, o `None` si se cerró.
        output: Option<ProgramOutputView>,
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
    /// El plan de renombrado en revisión cambió: se abrió, llegó el veredicto
    /// del core, se recorrió, o se cerró.
    AiRename {
        /// El plan, o `None` si se cerró.
        ai_rename: Option<AiRenameView>,
    },
    /// El plan de ORGANIZAR en revisión cambió (fase 8): se abrió, se
    /// recorrió, o se cerró. No tiene el tercer caso del de renombrar —«llegó
    /// el veredicto»— porque su token viaja con el plan.
    Organize {
        /// El plan, o `None` si se cerró.
        organize: Option<OrganizeView>,
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

/// Lo que el host le pide al PROCESO que lo hospeda, no al renderer.
///
/// Canal aparte, y no un `UiUpdate` más, por dos motivos que apuntan al mismo
/// sitio. El primero es de audiencia: esto lleva RUTAS y programas, y la
/// webview no tiene por qué verlos —ni tiene permiso para ejecutarlos: sus
/// capabilities son escuchar eventos y nada más (ADR 0066 D11)—. El segundo
/// es de responsabilidad: el host no lanza procesos ni toca el portapapeles;
/// dice QUÉ hay que hacer, con operandos que salen de su propio estado
/// semántico, y quien lo hospeda decide CÓMO con una puerta estrecha por
/// cosa. Un frontend que no sepa hacer alguna simplemente no la hace, y el
/// host se entera porque nadie le contesta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeEffect {
    /// Pon esto en el portapapeles.
    ///
    /// Ya compuesto —una ruta por línea, en su forma nativa cuando la
    /// tiene—: componerlo es una regla de presentación y vive donde vive el
    /// resto.
    CopyBytes {
        /// Lo que se copia, en BYTES y sin decodificar.
        ///
        /// Bytes y no `String` porque un nombre de fichero es bytes (regla
        /// 1): pasarlo por `from_utf8_lossy` metería el carácter de
        /// sustitución en el portapapeles, y lo que se pegue después abriría
        /// otro fichero —o ninguno—. El helper del sistema lo recibe por
        /// STDIN, que tampoco lo decodifica.
        bytes: Vec<u8>,
        /// Cuántas rutas lleva, para decirlo sin volver a contarlas.
        count: usize,
    },
    /// Abre ESTA entrada con la aplicación que el escritorio elija.
    OpenPath {
        /// Qué se abre. Es un `VPath`: quien lo hospeda lo convierte a ruta
        /// nativa —o dice que no puede, porque un `sftp://` no se le pasa a
        /// `xdg-open`—.
        path: norte_proto::VPath,
    },
    /// Saca un aviso por el ESCRITORIO (#285).
    ///
    /// El texto viaja YA COMPUESTO, traducido, enmascarado y acotado: una
    /// notificación sale del proceso y puede acabar en un historial o en la
    /// pantalla de bloqueo, así que lo que lleva dentro tiene que haber
    /// pasado por las mismas manos que lo que se pinta en la barra. Quien la
    /// entrega solo la entrega.
    Notify {
        /// La primera línea: qué pasó, en categoría.
        titulo: String,
        /// El detalle, con el nombre del fichero cuando lo hay.
        cuerpo: String,
    },
    /// Pide al ESCRITORIO que el lector elija un directorio (#284).
    ///
    /// Existe porque con un solo listado en pantalla no hay panel destino del
    /// que sacar el sitio, y rehusar la operación era dejar sin copiar a quien
    /// no ha partido la ventana. El selector lo pinta el sistema, no norte.
    ///
    /// **La ruta que vuelva es texto del renderer y se trata como tal**: el
    /// host la valida y, sobre todo, la ENSEÑA en la confirmación antes de
    /// mover un byte. Los operandos —qué se copia— siguen saliendo del estado
    /// del host y no del mensaje, que es la regla de ADR 0069.
    PickDirectory {
        /// Dónde abrir el selector: el directorio del panel activo. Es una
        /// sugerencia, no una restricción — el lector puede irse a otro sitio.
        desde: norte_proto::VPath,
    },
    /// Corre un PROGRAMA con estos argumentos (#312): suelto (`detached`,
    /// un comparador gráfico que abre su ventana) o ESPERÁNDOLO y
    /// capturando lo que imprima, que vuelve como
    /// `UiAction::ProgramFinished` y se enseña.
    ///
    /// El argv viene RESUELTO: el programa ya es una ruta absoluta (ADR
    /// 0082, antes de darle un `cwd`) y las rutas de los ficheros ya están
    /// interpoladas con las reglas compartidas (`[ui] diff`, `%F`). Quien
    /// hospeda no decide nada: lanza. En BYTES, porque un nombre de fichero
    /// es bytes (regla 1) y un argumento que no fuera UTF-8 abriría otro
    /// fichero o ninguno.
    RunProgram {
        /// Clave Fluent de lo que se está haciendo, para el panel.
        title_key: String,
        /// Programa (ruta absoluta) y argumentos, en bytes.
        argv: Vec<Vec<u8>>,
        /// Directorio de trabajo, en bytes nativos, si lo hay.
        cwd: Option<Vec<u8>>,
        /// `true` = lanzar y soltar; `false` = esperar y capturar.
        detached: bool,
    },
    /// Abre un terminal sentado en ESTE directorio.
    OpenTerminal {
        /// Dónde se sienta.
        dir: norte_proto::VPath,
    },
    /// El RELEVO a la TERMINAL (fase 9): la pantalla ya está escrita y la
    /// sesión, soltada; ahora lanza `ntc --attach` y CIERRA esta ventana.
    ///
    /// Va por este canal y no por la vista porque lanzar un proceso y cerrarse
    /// es de quien hospeda: el host no sabe abrir un emulador de terminal, ni
    /// debe. Lo que el host garantiza antes de emitirlo es lo que hace seguro
    /// el relevo — que la sesión está guardada y libre.
    ///
    /// Si el lanzamiento falla, quien hospeda lo dice y NO se cierra: la
    /// sesión está suelta pero la pantalla sigue aquí, que es el fallo
    /// barato.
    HandoffToTerminal {
        /// `true` si este proceso habla con el daemon, para que la terminal
        /// arranque igual. Sin él iría contra su core embebido y no
        /// encontraría la sesión que se acaba de soltar.
        daemon: bool,
    },
    /// El tema activo es ahora este: vuelve a resolver lo que salga de él.
    ///
    /// Va por ESTE canal y no por el de la vista porque el tema no cruza al
    /// renderer como datos: cruza convertido en lo que ese renderer sepa
    /// pintar —variables CSS en la webview, otra cosa en el siguiente— y esa
    /// conversión es de quien hospeda, no del host. El host dice qué tema
    /// hay; cómo se ve es de la casa.
    ///
    /// Existe porque lo que hospeda resuelve el tema UNA vez al arrancar. Sin
    /// esto, la ventana no podía cambiar de tema en marcha: ni desde su propio
    /// selector, ni al cambiar de perfil — que es la mitad de para lo que
    /// existe un perfil.
    ThemeChanged {
        /// Cómo se llama el tema que hay que resolver.
        name: String,
    },
    /// Ciérrate: el lector lo pidió y, si había que preguntar, ya se
    /// preguntó.
    ///
    /// Quien hospeda vuelca la sesión y destruye la ventana. Va por aquí y no
    /// por el gesto del gestor de ventanas porque la pregunta la decide el
    /// host: `[ui] confirm_quit` es configuración, y una ventana que se
    /// cierra sola con una copia a medias no es una ventana que obedece.
    CloseWindow,
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
