//! Lo que un PLUGIN pinta en un hueco: un marco de texto con estilo y zonas
//! pulsables (spec 2026-09-15, fase 3).
//!
//! El guest no dibuja: DESCRIBE. Manda líneas de [`crate::ansi::StyledSpan`]
//! —el mismo tramo que ya usan los previews estilados, con rol validado contra
//! el conjunto cerrado de `norte_theme::Role`— y una lista de [`Hit`], zonas
//! que al pulsarse ejecutan un COMANDO del catálogo. Nunca una acción libre:
//! un plugin no puede hacer por un clic nada que el lector no pudiera hacer
//! con una tecla, así que la policy queda intacta (regla dura 9).
//!
//! Las cotas son las del PROTOCOLO (`norte_proto::methods::PANEL_MAX_*`), y
//! aquí solo se reexportan: las dos superficies —el terminal y la ventana—
//! tienen que recortar IGUAL, y un marco que una acepta y la otra rechaza es
//! la divergencia que el ADR 0077 persigue. Escribir los números otra vez en
//! este crate sería tener dos que deben coincidir y nadie obliga a ello.

use crate::ansi::StyledSpan;

/// Tope de líneas de un marco.
///
/// Es la del PROTOCOLO, reexportada: un panel de ocho filas que manda mil
/// líneas describe algo que nadie va a leer, y el tope acota ese gasto sin
/// convertirlo en un error (recortar es fail-soft, como todo lo cosmético).
/// Declararla aquí otra vez sería el mismo número escrito en dos sitios, que
/// es exactamente lo que diverge al primer cambio.
pub use norte_proto::methods::PANEL_MAX_LINES as MAX_LINES;

/// Tope de tramos por línea. La del protocolo; ver [`MAX_LINES`].
pub use norte_proto::methods::PANEL_MAX_SPANS_PER_LINE as MAX_SPANS_PER_LINE;

/// Tope de zonas pulsables de un marco. La del protocolo; ver [`MAX_LINES`].
pub use norte_proto::methods::PANEL_MAX_HITS as MAX_HITS;

/// Una zona pulsable del marco: al pulsarla corre un comando del catálogo.
///
/// `row`/`col` son celdas DENTRO del marco, no de la pantalla: quien pinta
/// sabe dónde cayó el hueco y hace la cuenta. `width` es cuántas celdas ocupa
/// a lo ancho desde `col`; una zona de alto mayor que una fila se describe con
/// un `Hit` por fila, que es lo que evita tener que definir solapes en dos
/// dimensiones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Fila del marco donde empieza, contando desde cero.
    pub row: u16,
    /// Columna donde empieza, contando desde cero.
    pub col: u16,
    /// Cuántas celdas ocupa a lo ancho. Cero = no se puede pulsar.
    pub width: u16,
    /// El comando del catálogo que ejecuta. Si no existe, no pasa nada: lo
    /// resuelve el despacho normal, que ya sabe decir «aquí no».
    pub command: String,
    /// Su argumento, si lo lleva (un directorio, un nombre).
    pub arg: Option<String>,
}

/// Un marco pintable: líneas con estilo y zonas pulsables.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StyledFrame {
    /// Las líneas, de arriba abajo. Una línea vacía es una línea en blanco.
    pub lines: Vec<Vec<StyledSpan>>,
    /// Las zonas pulsables, en el orden en que llegaron.
    pub hits: Vec<Hit>,
}

impl StyledFrame {
    /// Construye un marco ACOTADO a partir de lo que un guest mandó.
    ///
    /// Recorta líneas, tramos y zonas a los topes de este módulo, y tira las
    /// zonas que apunten a una fila que el recorte se llevó: un `Hit` sobre
    /// una línea que no se pinta es una zona invisible que ejecuta algo, que
    /// es peor que no tenerla.
    ///
    /// ```
    /// use norte_frontend::frame::{Hit, StyledFrame};
    ///
    /// let f = StyledFrame::clamped(
    ///     Vec::new(),
    ///     vec![Hit { row: 3, col: 0, width: 4, command: "nav.enter".to_owned(), arg: None }],
    /// );
    /// assert!(f.hits.is_empty(), "sin líneas no hay dónde pulsar");
    /// ```
    #[must_use]
    pub fn clamped(mut lines: Vec<Vec<StyledSpan>>, hits: Vec<Hit>) -> Self {
        lines.truncate(MAX_LINES);
        for linea in &mut lines {
            linea.truncate(MAX_SPANS_PER_LINE);
        }
        let alto = lines.len();
        let hits: Vec<Hit> = hits
            .into_iter()
            .filter(|h| usize::from(h.row) < alto && h.width > 0)
            .take(MAX_HITS)
            .collect();
        Self { lines, hits }
    }

    /// La zona pulsable que hay en esa celda del marco, si la hay.
    ///
    /// La PRIMERA que case, que es el orden en que el guest las mandó: dos
    /// zonas solapadas son un error del guest, y elegir la primera es una
    /// regla que se puede explicar — elegir «la más pequeña» o «la última»
    /// pediría que el lector adivinara cuál.
    ///
    /// ```
    /// use norte_frontend::frame::{Hit, StyledFrame};
    ///
    /// let f = StyledFrame::clamped(
    ///     vec![Vec::new()],
    ///     vec![Hit { row: 0, col: 2, width: 3, command: "pane.reload".to_owned(), arg: None }],
    /// );
    /// assert!(f.hit_at(0, 1).is_none());
    /// assert_eq!(f.hit_at(0, 4).map(|h| h.command.as_str()), Some("pane.reload"));
    /// assert!(f.hit_at(0, 5).is_none());
    /// ```
    #[must_use]
    pub fn hit_at(&self, row: u16, col: u16) -> Option<&Hit> {
        self.hits
            .iter()
            // El fin se calcula en `u32`: con `saturating_add`, una zona
            // pegada al tope del espacio de coordenadas perdía su última
            // celda —el fin saturaba en `u16::MAX` y la comparación es
            // exclusiva—, así que el borde derecho del marco no se podía
            // pulsar.
            .find(|h| {
                h.row == row
                    && col >= h.col
                    && u32::from(col) < u32::from(h.col) + u32::from(h.width)
            })
    }

    /// Cuántas filas ocupa.
    #[must_use]
    pub fn height(&self) -> usize {
        self.lines.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(row: u16, col: u16, width: u16) -> Hit {
        Hit {
            row,
            col,
            width,
            command: "nav.enter".to_owned(),
            arg: None,
        }
    }

    fn span(text: &str) -> StyledSpan {
        StyledSpan {
            text: text.to_owned(),
            role: None,
            fg: None,
            bg: None,
        }
    }

    #[test]
    fn las_cotas_recortan_lineas_tramos_y_zonas() {
        let lines = vec![vec![span("x"); MAX_SPANS_PER_LINE + 10]; MAX_LINES + 10];
        let hits = (0..u16::try_from(MAX_HITS + 10).expect("cabe"))
            .map(|i| hit(i, 0, 1))
            .collect();
        let f = StyledFrame::clamped(lines, hits);
        assert_eq!(f.lines.len(), MAX_LINES);
        assert_eq!(f.lines[0].len(), MAX_SPANS_PER_LINE);
        assert_eq!(f.hits.len(), MAX_HITS);
    }

    /// Una zona que apunta a una fila recortada se va con ella.
    ///
    /// Si se quedara, el marco tendría una celda que ejecuta algo y no enseña
    /// nada: un botón invisible, que es peor que un botón que falta.
    #[test]
    fn una_zona_sobre_una_fila_que_no_se_pinta_se_tira() {
        let f = StyledFrame::clamped(vec![vec![span("a")], vec![span("b")]], vec![hit(5, 0, 3)]);
        assert!(f.hits.is_empty());
    }

    /// Ancho cero no es una zona: es una coordenada.
    #[test]
    fn una_zona_sin_ancho_no_se_puede_pulsar() {
        let f = StyledFrame::clamped(vec![vec![span("a")]], vec![hit(0, 0, 0)]);
        assert!(f.hits.is_empty());
    }

    #[test]
    fn la_primera_zona_que_case_gana() {
        let mut a = hit(0, 0, 10);
        a.command = "primera".to_owned();
        let mut b = hit(0, 2, 2);
        b.command = "segunda".to_owned();
        let f = StyledFrame::clamped(vec![vec![span("hola")]], vec![a, b]);
        assert_eq!(f.hit_at(0, 3).map(|h| h.command.as_str()), Some("primera"));
    }

    /// El borde derecho es EXCLUSIVO: una zona de tres celdas desde la 2 cubre
    /// 2, 3 y 4, y no la 5.
    #[test]
    fn el_borde_derecho_de_una_zona_no_entra() {
        let f = StyledFrame::clamped(vec![vec![span("hola")]], vec![hit(0, 2, 3)]);
        assert!(f.hit_at(0, 1).is_none());
        assert!(f.hit_at(0, 2).is_some());
        assert!(f.hit_at(0, 4).is_some());
        assert!(f.hit_at(0, 5).is_none());
    }

    /// Una zona al final del espacio de coordenadas no desborda al sumar.
    #[test]
    fn una_zona_pegada_al_tope_no_desborda() {
        let f = StyledFrame::clamped(vec![vec![span("a")]], vec![hit(0, u16::MAX - 1, 10)]);
        assert!(f.hit_at(0, u16::MAX).is_some());
    }
}
