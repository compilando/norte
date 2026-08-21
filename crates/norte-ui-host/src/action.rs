//! Lo que el renderer PIDE.
//!
//! Son acciones SEMÁNTICAS, no métodos del backend: «mueve el cursor», no
//! «llama a `fs.list` con este cursor». La diferencia importa porque lo que
//! se expone es lo que un renderer puede hacer, y un renderer no debe poder
//! pedir un `rpc(method, params)` arbitrario (ADR 0066, decisión D11).
//!
//! Ninguna acción nombra un path. Se actúa sobre filas por su [`RowKey`], y
//! toda acción que nombre una fila lleva TAMBIÉN la generación en la que el
//! renderer la vio. Sin ese par la clave no dice nada: es un índice, y un
//! índice de una pantalla anterior nombra otro fichero. El host compara la
//! generación con la época del listado y responde
//! [`crate::ActionAck::Stale`] cuando no coinciden — que es lo que impide que
//! un click tardío actúe sobre lo que ocupó esa fila DESPUÉS.
//!
//! Y ninguna acción acepta una cadena de ruta, ni la aceptará: lo que el
//! renderer puede nombrar es lo que el host le dio.

use serde::{Deserialize, Serialize};

use crate::bridge::{ModalId, RowKey};
use crate::keys::KeyInput;

/// Una petición del renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum UiAction {
    /// Mueve el cursor del hueco. `delta` en filas; negativo hacia arriba.
    ///
    /// Es la acción que más se repite (una tecla mantenida) y el host NO la
    /// fusiona: aplica una por una y emite un parche de cursor por cada una.
    /// Con el renderer de referencia no hay nada que fusionar —serializa sus
    /// llamadas, así que como mucho hay una en el buzón—, y fusionar sin
    /// necesidad complica el punto donde se contestan los acuses. Un renderer
    /// que mande en lotes hará que valga la pena; hasta entonces, esto
    /// describe lo que pasa y no lo que estaría bien.
    MoveCursor {
        /// Hueco.
        slot_id: u32,
        /// Filas a mover.
        delta: i64,
    },
    /// Pone el cursor en una fila concreta (un click).
    SelectRow {
        /// Hueco.
        slot_id: u32,
        /// Fila.
        key: RowKey,
        /// La generación en la que el renderer vio esa fila.
        generation: u64,
    },
    /// Marca o desmarca una fila.
    ToggleMark {
        /// Hueco.
        slot_id: u32,
        /// Fila.
        key: RowKey,
        /// La generación en la que el renderer vio esa fila.
        generation: u64,
    },
    /// Marca TODO el rango entre dos filas, extremos incluidos.
    ///
    /// Un barrido con el ratón (shift+click, arrastre) es UNA acción y no una
    /// ristra de `ToggleMark`: qué entra en un rango —y qué no, como `..`—
    /// es una regla de selección, y esas viven en `norte-frontend`, no en el
    /// renderer (ADR 0066, decisión D14). El orden de los extremos da igual.
    MarkRange {
        /// Hueco.
        slot_id: u32,
        /// Un extremo.
        from: RowKey,
        /// El otro.
        to: RowKey,
        /// La generación en la que el renderer vio esas filas.
        generation: u64,
    },
    /// Abre lo que haya bajo esa fila: entra en el directorio, o abre el
    /// fichero por el camino de siempre.
    Activate {
        /// Hueco.
        slot_id: u32,
        /// Fila.
        key: RowKey,
        /// La generación en la que el renderer vio esa fila.
        generation: u64,
    },
    /// Sube al directorio padre.
    Parent {
        /// Hueco.
        slot_id: u32,
    },
    /// Atrás y adelante en el rastro de navegación.
    History {
        /// Hueco.
        slot_id: u32,
        /// `true` = atrás.
        back: bool,
    },
    /// La ventana visible cambió (scroll o resize).
    ///
    /// Llega DEBOUNCED desde el renderer: el pintado del scroll es suyo, y lo
    /// único que cruza es qué filas hacen falta.
    SetVisibleRange {
        /// Hueco.
        slot_id: u32,
        /// Primera fila visible.
        first: u64,
        /// Cuántas caben.
        count: u32,
    },
    /// Ordena el listado por una columna (un click en su cabecera).
    ///
    /// La columna va por su ID, no por su posición ni por su etiqueta: qué
    /// significa ordenar por ella —y si se invierte o empieza de nuevo— lo
    /// decide la regla compartida (`SortSpec::after_click`), no el renderer.
    SortBy {
        /// Hueco.
        slot_id: u32,
        /// Id de la columna, tal como viajó en su cabecera.
        column: String,
    },
    /// Cambia el foco de teclado de hueco.
    FocusSlot {
        /// Hueco.
        slot_id: u32,
    },
    /// Responde a un diálogo.
    ///
    /// El `choice` es uno de los ids que el propio diálogo publicó. Un id que
    /// no esté en la lista no se interpreta: no hay respuestas implícitas.
    Dialog {
        /// Diálogo.
        id: ModalId,
        /// Respuesta elegida.
        choice: String,
    },
    /// Teclea en el campo de texto del diálogo abierto.
    DialogInput {
        /// Diálogo.
        id: ModalId,
        /// Texto completo tras la edición (no un delta: el renderer es dueño
        /// del caret, y mandar el texto entero evita reconstruirlo en Rust).
        text: String,
    },
    /// Pide cancelar una task.
    CancelTask {
        /// Id de la task.
        task_id: u64,
    },
    /// El tamaño de la ventana cambió.
    ///
    /// En CELDAS de layout, no en píxeles: los mínimos de cada panel están
    /// declarados así y se comparten con el TUI, de modo que «esto no cabe»
    /// significa lo mismo en las dos superficies. Redimensionar reparte otra
    /// vez; jamás reescribe la disposición guardada, que es la intención del
    /// usuario y no una función del tamaño de su ventana.
    SetViewport {
        /// Ancho en celdas.
        width: u16,
        /// Alto en celdas.
        height: u16,
    },
    /// Una tecla.
    ///
    /// El renderer manda la tecla NORMALIZADA y nada más: quién resuelve un
    /// contador, un prefijo a medias o qué comando lleva ligado es Rust, con
    /// el mismo resolver y los mismos presets que el TUI. Dos keymaps serían
    /// dos sitios donde divergir sin que nadie lo note.
    Key(KeyInput),
    /// Cuántas líneas caben en el visor.
    ///
    /// El host no puede saberlo: su rejilla son celdas de disposición y el
    /// cromo del visor lo pinta el renderer. Adivinarlo hacía dos cosas mal a
    /// la vez —mandar más líneas de las que caben, que se recortan sin
    /// decirlo, y avanzar una página por un número distinto del que se ve—,
    /// así que cada página saltaba en silencio lo recortado.
    SetViewerRows {
        /// Líneas visibles.
        rows: u32,
    },
    /// Pone el cursor de la lateral de la ayuda en esa fila y ENSEÑA lo que
    /// haya (un click).
    ///
    /// Enseñar y no navegar, que es lo que hace la misma tecla de flecha:
    /// recorrer el índice no debe dejarle al lector un paso de vuelta que
    /// tenga que deshacer con `⌫` antes de poder cerrar. Una cabecera de
    /// grupo y una fila fuera de rango no hacen nada.
    HelpSelectTopic {
        /// Fila de la lateral, tal como viajó en el orden de `sidebar`.
        row: u32,
    },
    /// Actúa sobre una fila ejecutable del cuerpo de la ayuda (un click):
    /// corre el comando, o abre la página enlazada.
    ///
    /// Va por el MISMO camino que `enter`, y ese por el mismo que una tecla:
    /// la ayuda es otra puerta al catálogo, no un segundo despachador.
    HelpActivate {
        /// Índice dentro de `actions`.
        index: u32,
    },
    /// Pide un snapshot completo: el renderer perdió el hilo de la secuencia.
    Resync,
}
