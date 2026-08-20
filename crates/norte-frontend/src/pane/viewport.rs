//! La ventana: cuántas filas pintó el frame anterior y qué se deduce de eso.
//!
//! La paginación y el radio de la sonda de `stat` salen de aquí y no de
//! constantes, que mentirían en cualquier terminal o ventana que no midiera
//! justo eso.

use super::{DEFAULT_PAGE, EntryKind, PaneState, VPath};

impl PaneState {
    /// Filas de listado que este pane pintó en el ÚLTIMO frame (#124): el
    /// alto real lo decide el widget al pintar, así que el frontend lo
    /// devuelve aquí y el modelo deja de adivinarlo. `None` hasta el primer
    /// frame (o si el pane no se pintó: con el visor abierto, p. ej.).
    pub fn set_viewport_rows(&mut self, rows: usize) {
        self.viewport_rows = (rows > 0).then_some(rows);
    }

    /// Deja la ventana lista para pintar `rows` filas con el cursor donde
    /// está: fija el alto y ARRASTRA el desplazamiento solo si el cursor se ha
    /// salido.
    ///
    /// Es la regla de un gestor ortodoxo, y la de cualquier lista con la que
    /// el usuario ya tiene los dedos hechos: bajar dentro de la pantalla NO
    /// mueve el contenido; tocar el borde inferior lo mueve UNA fila; y al
    /// volver hacia arriba pasa lo simétrico. Lo que había antes era una
    /// función pura del cursor, así que el cursor vivía clavado en la última
    /// fila y el contenido se movía siempre.
    ///
    /// También reencuadra sin que el cursor se mueva: un listado que encoge
    /// —una recarga, un filtro— o una terminal que se hace más alta dejarían
    /// la ventana apuntando más allá del final, con filas en blanco debajo de
    /// contenido que sí existe.
    pub fn reconcile_viewport(&mut self, rows: usize) {
        self.set_viewport_rows(rows);
        self.viewport_offset = crate::viewport::sticky_offset(
            self.viewport_offset,
            self.cursor,
            self.entries.len(),
            rows,
        );
    }

    /// La primera fila visible del listado — ver [`Self::reconcile_viewport`].
    #[must_use]
    pub fn viewport_offset(&self) -> usize {
        self.viewport_offset
    }

    /// Filas visibles del último frame (#124) — ver [`Self::set_viewport_rows`].
    #[must_use]
    pub fn viewport_rows(&self) -> Option<usize> {
        self.viewport_rows
    }

    /// Cuántas filas mueve una página (#124): una PANTALLA menos una fila de
    /// contexto, como los gestores ortodoxos — nunca menos de una. Sin frame
    /// pintado todavía cae a [`DEFAULT_PAGE`].
    #[must_use]
    pub fn page_step(&self) -> usize {
        self.viewport_rows
            .map_or(DEFAULT_PAGE, |r| r.saturating_sub(1).max(1))
    }

    /// Paths candidatos a [`Self::hydrate`] en la ventana VISIBLE: entradas
    /// `File` sin `size` a `radius` filas del cursor (#52, listado lazy).
    /// Modelo COMPARTIDO por los dos frontends (regla 7): sondear solo la
    /// entrada ENFOCADA dejaba las columnas Tamaño/Fecha en blanco en todas
    /// las demás filas, que es justo lo que un gestor ortodoxo tiene que
    /// enseñar.
    ///
    /// El radio sale del alto REAL del último frame
    /// ([`Self::set_viewport_rows`], #124) y cae a `fallback` mientras no
    /// haya frame. Un radio igual al alto CUBRE la pantalla entera sea cual
    /// sea el scroll —lo visible siempre cae dentro de `cursor ± alto`— y de
    /// paso pre-carga una pantalla en cada sentido, así que desplazarse no
    /// estrena celdas en blanco. Un `Dir` jamás se sondea (su celda de
    /// tamaño va en blanco a propósito) y una entrada ya hidratada deja de
    /// ser candidata sola — el caller no necesita llevar más estado que la
    /// dedup de los que YA pidió (un stat fallido, si no, se reintenta en
    /// bucle).
    #[must_use]
    pub fn needs_stat_window(&self, fallback: usize) -> Vec<VPath> {
        let radius = self.viewport_rows.unwrap_or(fallback);
        let lo = self.cursor.saturating_sub(radius);
        let hi = self.cursor.saturating_add(radius).saturating_add(1);
        self.needs_stat_at(lo..hi)
    }

    /// [`Self::needs_stat_window`] sobre índices ABSOLUTOS explícitos: el
    /// frontend que conoce su rango visible EXACTO no tiene que aproximarlo
    /// con un radio alrededor del cursor. La GUI lo recibe de `uniform_list`
    /// (que solo pide las filas que va a pintar), así que con un scroll de
    /// rueda —que mueve la ventana SIN mover el cursor— sigue hidratando lo
    /// que se ve. Índices fuera del listado se ignoran.
    #[must_use]
    pub fn needs_stat_at(&self, indices: impl IntoIterator<Item = usize>) -> Vec<VPath> {
        indices
            .into_iter()
            .filter_map(|i| self.entries.get(i))
            .filter(|e| e.kind == EntryKind::File && e.size.is_none())
            .map(|e| e.path.clone())
            .collect()
    }
}
