//! La línea de tiempo del journal en la TUI (fase 7 del programa WOW): el
//! hueco, lo que se pinta en él y qué pasa al deshacer hasta un punto.
//!
//! El MODELO —qué es una fila, qué agrupa un lote, qué corte conserva la
//! fila señalada y cuántas entradas se va a llevar— vive en
//! [`norte_frontend::timeline`], compartido con la ventana. Aquí está lo que
//! sólo este frontend sabe: dónde cae el hueco, cómo se pinta un punto y qué
//! tecla hace qué.

/// El `kind` del hueco, tal y como lo declara el registro compartido.
pub const KIND: &str = "timeline";

/// Cuántas filas se piden por página.
///
/// Muy por debajo del tope del protocolo (200): esto es una pantalla que se
/// lee, y lo que no quepa se pide al llegar abajo. Pedir la página máxima de
/// entrada sería traer doscientas filas para enseñar diez.
pub const POR_PAGINA: u32 = 50;
