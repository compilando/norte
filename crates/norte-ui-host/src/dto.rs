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
    /// El visor, si hay uno abierto. Ocupa la pantalla: mientras esté, las
    /// teclas son suyas y el listado no se mueve por debajo.
    pub viewer: Option<ViewerView>,
    /// Idioma negociado, para que el renderer pida el catálogo correcto.
    pub locale: String,
}

/// Lo que el visor enseña.
///
/// Cinco banderas y no un estado: cada una es un HECHO independiente que el
/// host resolvió (es hexadecimal, el encoding lo forzó el usuario, la
/// decodificación tuvo errores, el fichero seguía, el nombre difiere del
/// real), y juntarlas en un enum obligaría a inventar combinaciones que no
/// existen.
#[allow(clippy::struct_excessive_bools)]
///
/// El texto viene DECODIFICADO y en líneas por `norte_frontend::viewer`, que
/// es el mismo modelo que pinta el TUI: la detección de encoding, el salto a
/// hexadecimal de un binario y el recorte de la ventana visible son suyos, no
/// del renderer.
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
    /// El diálogo pide texto libre, y esto es lo tecleado hasta ahora.
    pub input: Option<String>,
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
