//! Lo que el renderer PIDE.
//!
//! Son acciones SEMÁNTICAS, no métodos del backend: «mueve el cursor», no
//! «llama a `fs.list` con este cursor». La diferencia importa porque lo que
//! se expone es lo que un renderer puede hacer, y un renderer no debe poder
//! pedir un `rpc(method, params)` arbitrario (ADR 0066, decisión D11).
//!
//! Ninguna acción nombra un path. Se actúa sobre filas por su [`RowKey`], que
//! es opaca y caduca con la generación de su hueco.

use serde::{Deserialize, Serialize};

use crate::bridge::{ModalId, RowKey};

/// Una petición del renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum UiAction {
    /// Mueve el cursor del hueco. `delta` en filas; negativo hacia arriba.
    ///
    /// Es la acción que más se repite (una tecla mantenida), así que el host
    /// la fusiona: lo que importa es dónde acaba el cursor, no cuántas veces
    /// se pidió.
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
    },
    /// Marca o desmarca una fila.
    ToggleMark {
        /// Hueco.
        slot_id: u32,
        /// Fila.
        key: RowKey,
    },
    /// Abre lo que haya bajo esa fila: entra en el directorio, o abre el
    /// fichero por el camino de siempre.
    Activate {
        /// Hueco.
        slot_id: u32,
        /// Fila.
        key: RowKey,
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
    /// Pide un snapshot completo: el renderer perdió el hilo de la secuencia.
    Resync,
}
