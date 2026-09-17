//! El mapa de disco, en la ventana (fase 4).
//!
//! El estado es el COMPARTIDO (`norte_frontend::diskmap`), el mismo que usa el
//! terminal: qué directorio se describe, lo medido y cuál es el hijo elegido.
//! Y el reparto en rectángulos es el compartido también
//! (`norte_frontend::treemap::squarify`). Aquí solo queda proyectarlo a lo que
//! el renderer pinta.
//!
//! # Por qué reparte el HOST y no el renderer
//! Un treemap calculado dos veces son dos treemaps distintos en cuanto alguien
//! toque un redondeo, y entonces el rectángulo que se pinta y el que resuelve
//! un clic dejan de ser el mismo — o sea, pulsas uno y se abre el de al lado.
//! Misma regla que el panel de plugin (ADR 0077), y aquí con más motivo:
//! lo que hay al otro lado de un clic es un fichero.

use norte_frontend::layout::SlotId;

use super::Estado;
use crate::bridge::clamp_display;

/// El kind que ocupa un hueco de mapa de disco.
pub(super) const KIND: &str = "disk-map";

impl Estado {
    /// Proyecta el mapa de disco de un hueco a lo que el renderer pinta.
    ///
    /// El marco se reparte con el tamaño de DENTRO del borde, igual que la
    /// firma de un panel de plugin: quien describe el contenido no sabe dónde
    /// cayó su hueco, así que la cuenta la hace quien pinta — y aquí el host
    /// pinta y resuelve, de modo que las dos cuentas son la misma por
    /// construcción.
    ///
    /// Sin hueco colocado no hay tamaño, y entonces no hay mapa: se manda
    /// vacío con su título, como un panel cuyo primer marco no ha llegado.
    pub(super) fn vista_de_mapa(&self, id: u32) -> crate::dto::DiskMapSlotView {
        let mapa = self.mapas.get(&id);
        // El título es el NOMBRE del directorio que se describe, no su ruta:
        // el hueco es estrecho y la ruta entera no cabe. Sale de un nombre de
        // fichero, así que se enmascara como cualquier otro.
        let (title, title_hostile) = mapa.and_then(|m| m.dir()).map_or_else(
            || (String::new(), false),
            |d| {
                d.file_name().map_or_else(
                    // La raíz de un provider no tiene nombre base: se dice con
                    // su esquema en vez de dejar el título en blanco.
                    || (d.scheme().to_owned(), false),
                    |n| norte_frontend::display_name(n.as_bytes()),
                )
            },
        );

        let celdas = self
            .reparto
            .placements
            .iter()
            .find(|(SlotId(s), _)| *s == id)
            .map(|(_, r)| (r.width.saturating_sub(2), r.height.saturating_sub(2)));

        let (lines, hits) = match (mapa, celdas) {
            (Some(m), Some((cols, rows))) => {
                let marco = norte_frontend::treemap::squarify(&m.informe().children, cols, rows);
                let lines = marco
                    .lines
                    .iter()
                    .map(|linea| linea.iter().map(super::views::span_view).collect())
                    .collect();
                let hits = marco
                    .hits
                    .iter()
                    .map(|h| crate::dto::HitView {
                        row: h.row,
                        col: h.col,
                        width: h.width,
                    })
                    .collect();
                (lines, hits)
            }
            _ => (Vec::new(), Vec::new()),
        };

        crate::dto::DiskMapSlotView {
            slot_id: id,
            title: clamp_display(title),
            title_hostile,
            lines,
            hits,
            measuring: matches!(
                mapa.map(norte_frontend::diskmap::DiskMap::estado),
                Some(norte_frontend::diskmap::Estado::Midiendo(_))
            ),
        }
    }
}

// PENDIENTE (T5): podar `mapas` cuando el árbol cambie, como hace
// `sondear_paneles` con el estado opaco de los plugins — un `SlotId` se
// reutiliza, y el mapa de un hueco nuevo heredaría lo medido del anterior: los
// tamaños de otro directorio bajo este título. Todavía no hace falta porque
// NADIE llena `mapas`: la ventana declara el hueco y lo pinta, pero medir
// —pedir `fs.dir_usage` y aterrizar su informe— es lo que queda de T5. La poda
// entra con quien lo llene, no antes: una poda sin nada que podar es código
// muerto que el día de mañana nadie sabe si se llegó a llamar.
