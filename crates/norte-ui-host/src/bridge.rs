//! El sobre en el que viaja TODO lo que cruza al renderer, y sus topes.
//!
//! Un renderer —una webview, un shell Flutter, un test headless— no comparte
//! memoria con el host: recibe mensajes. Este módulo dice cómo son esos
//! mensajes y qué se le exige a cada uno, que es lo que hace que un renderer
//! escrito por otro no pueda interpretar medio mensaje y seguir como si nada.

use serde::{Deserialize, Serialize};

/// Versión del contrato del bridge.
///
/// No es la del protocolo del daemon: son dos fronteras distintas y se mueven
/// por motivos distintos. Un renderer que no reconoce esta versión NO
/// interpreta el mensaje: enseña una pantalla de incompatibilidad (ADR 0066).
///
/// - **2**: el snapshot lleva el reparto de la pantalla
///   ([`crate::dto::LayoutView`]) y va COMPLETO (diálogos y tablero
///   incluidos); un cambio de foco viaja como parche y no como foto.
/// - **1**: el contrato inicial de la fase 2.
pub const BRIDGE_VERSION: u32 = 2;

/// Tope de una cadena que cruza al renderer, en bytes.
///
/// Todo lo pintable está acotado en Rust y no en el renderer: un nombre
/// hostil de 700 KB no puede convertirse en el problema de quien pinta.
pub const MAX_STRING_BYTES: usize = 4096;

/// Filas que puede llevar UN mensaje.
pub const MAX_ROWS_PER_BATCH: usize = 2048;

/// Avisos vivos a la vez; los más viejos se caen.
pub const MAX_NOTICES: usize = 32;

/// Tasks proyectadas a la vez.
pub const MAX_TASKS: usize = 256;

/// Bytes de una previsualización que cruzan al renderer.
pub const MAX_PREVIEW_BYTES: usize = 256 * 1024;

/// Identidad de UNA instancia del host.
///
/// Ordena todo lo demás: una `sequence` solo significa algo dentro de la
/// instancia que la emitió, y una acción que llega con otra instancia es de
/// una vida anterior del host (un reattach tras reiniciar) y no muta nada.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InstanceId(String);

impl InstanceId {
    /// Construye la identidad. La fabrica el host al arrancar.
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// La cadena opaca.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// La clave de una FILA, opaca para el renderer.
///
/// Solo vale dentro de `(instancia, hueco, generación)`. Cuando el host
/// re-lista, la generación sube: un click que llega con la anterior se
/// responde [`StaleAction::Generation`] y no hace nada. Es lo que impide que
/// un doble click tardío actúe sobre el fichero que ocupó esa fila DESPUÉS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RowKey(pub u64);

/// La identidad de un diálogo abierto.
///
/// Confirmar es idempotente por esto: un segundo `Confirm` con el mismo id no
/// vuelve a lanzar la operación, y uno con un id viejo no cierra el diálogo
/// que hay AHORA.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModalId(pub u64);

/// El testigo de una petición en vuelo.
///
/// Toda respuesta asíncrona lo lleva, y la respuesta de un testigo que ya no
/// interesa se descarta EN RUST, no se esconde en el renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestToken(pub u64);

/// El sobre de todo mensaje del host hacia el renderer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgeEnvelope<T> {
    /// Versión del contrato ([`BRIDGE_VERSION`]).
    pub bridge_version: u32,
    /// Quién emite.
    pub instance_id: InstanceId,
    /// Orden dentro de esta instancia. Empieza en 0 y no salta.
    pub sequence: u64,
    /// Lo que se envía.
    pub payload: T,
}

impl<T> BridgeEnvelope<T> {
    /// Mete `payload` en un sobre de ESTA versión.
    pub fn new(instance_id: InstanceId, sequence: u64, payload: T) -> Self {
        Self {
            bridge_version: BRIDGE_VERSION,
            instance_id,
            sequence,
            payload,
        }
    }

    /// ¿Puede este renderer interpretar el sobre?
    ///
    /// Es una pregunta de todo o nada a propósito: media interpretación de un
    /// contrato que no se conoce es peor que una pantalla que lo dice.
    #[must_use]
    pub fn is_supported(&self) -> bool {
        self.bridge_version == BRIDGE_VERSION
    }
}

/// Por qué una acción no hizo nada, sin que sea un error.
///
/// Las tres son carreras normales entre un renderer que pinta y un host que
/// ya cambió de estado, y ninguna es culpa de nadie: se responden y se
/// ignoran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleAction {
    /// La acción venía de OTRA instancia del host.
    Instance,
    /// La fila (o el hueco) es de una generación anterior: hubo un re-listado.
    Generation,
    /// El diálogo al que responde ya no está abierto.
    Modal,
}

/// La respuesta a una acción.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum ActionAck {
    /// Aplicada. `sequence` es la primera actualización que la refleja.
    Applied {
        /// La actualización en la que se verá.
        sequence: u64,
    },
    /// No se aplicó, y no pasa nada: ver [`StaleAction`].
    Stale {
        /// Cuál de las tres carreras fue.
        reason: StaleAction,
    },
    /// La acción no está disponible AHORA (comando atenuado, sin permiso,
    /// sin conexión). Lleva la clave Fluent del motivo, no la frase: quien
    /// traduce es el renderer con el catálogo del host.
    Unavailable {
        /// Clave Fluent del porqué.
        reason_key: String,
    },
}

/// Recorta una cadena al tope del bridge SIN partir un carácter.
///
/// Se recorta en la frontera de display y se DICE (`…`), que es la misma
/// regla que el resto del proyecto aplica a lo pintable: nunca se pierde algo
/// en silencio.
#[must_use]
pub fn clamp_display(mut s: String) -> String {
    if s.len() <= MAX_STRING_BYTES {
        return s;
    }
    let mut corte = MAX_STRING_BYTES.saturating_sub('…'.len_utf8());
    while corte > 0 && !s.is_char_boundary(corte) {
        corte -= 1;
    }
    s.truncate(corte);
    s.push('…');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_sobre_de_otra_version_no_se_interpreta() {
        let mut e = BridgeEnvelope::new(InstanceId::new("i"), 0, 7u32);
        assert!(e.is_supported());
        e.bridge_version = BRIDGE_VERSION + 1;
        assert!(!e.is_supported(), "una versión futura NO se interpreta");
    }

    #[test]
    fn una_cadena_larga_se_recorta_y_se_dice() {
        let larga = "a".repeat(MAX_STRING_BYTES * 2);
        let out = clamp_display(larga);
        assert!(out.len() <= MAX_STRING_BYTES);
        assert!(out.ends_with('…'), "el recorte se ve");
    }

    /// El recorte jamás parte un carácter multibyte por la mitad.
    #[test]
    fn el_recorte_respeta_los_caracteres() {
        let larga = "é".repeat(MAX_STRING_BYTES);
        let out = clamp_display(larga);
        assert!(out.len() <= MAX_STRING_BYTES);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    /// Una cadena que ya cabe no se toca (ni se le añade el aviso).
    #[test]
    fn lo_que_cabe_viaja_intacto() {
        let s = String::from("café.txt");
        assert_eq!(clamp_display(s.clone()), s);
    }
}
