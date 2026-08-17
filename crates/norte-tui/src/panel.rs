//! El estado de panel del TUI y los huecos bien conocidos del preset
//! `orthodox`.
//!
//! # Por qué `PaneSlots` y no 213 sitios reescritos
//!
//! El objetivo de L1a es que el estado de los paneles deje de vivir en un
//! campo con nombre (`App.panes: [Pane; 2]`) y pase a estar indexado por
//! `SlotId`. Reescribir los 213 sitios que dicen `app.panes[i]` conseguiría
//! eso, y de paso metería 213 oportunidades de cambiar el comportamiento sin
//! querer en el mismo commit que dice no cambiarlo.
//!
//! [`PaneSlots`] consigue lo mismo con un adaptador: por dentro es un
//! [`SlotStore`] indexado por [`SlotId`]; por fuera se indexa con `0`/`1`, se
//! itera y se intercambia igual que el array de antes. El almacenamiento SÍ
//! cambia; la ergonomía de los sitios de llamada no. Cuando L1b los haga
//! conscientes de los huecos, lo harán uno a uno y con su propia revisión.

use std::ops::{Index, IndexMut};

use norte_frontend::layout::{
    Dir, KindId, Node, Params, Rect as LayoutRect, Size, SlotId, SlotStore,
};

use crate::app::Pane;

/// El hueco del pane izquierdo en el preset `orthodox`.
///
/// Fijos y no acuñados: son los mismos huecos que el TUI ha tenido siempre,
/// así que el estado de hoy y el preset son la misma cosa y no hay migración
/// que hacer.
pub const SLOT_LEFT: SlotId = SlotId(1);
/// El hueco del pane derecho en el preset `orthodox`.
pub const SLOT_RIGHT: SlotId = SlotId(2);
/// El hueco de la franja de tareas en el preset `orthodox`.
pub const SLOT_TASKS: SlotId = SlotId(3);
/// El hueco de la barra de estado en el preset `orthodox`.
pub const SLOT_STATUS: SlotId = SlotId(4);

/// Lo que puede haber dentro de un hueco en el TUI.
///
/// En L1a solo hay dos variantes vivas: el listado, y la caja de un kind que
/// este binario no sabe pintar. El visor, la comparación y la sincronización
/// siguen siendo campos de `App` hasta L1b — moverlos no hace falta para que
/// el motor entre, y mezclarlo aquí sería riesgo sin contrapartida.
#[derive(Debug)]
pub enum TuiPanel {
    /// Un listado de ficheros.
    Browser(Box<Pane>),
    /// Un kind que este binario no conoce: se pinta como una caja con su
    /// nombre y sus `params` se conservan intactos, para que abrir el layout
    /// de la GUI en el TUI no le borre nada.
    Unknown {
        /// El kind que no se supo pintar.
        kind: KindId,
        /// Sus parámetros, tal cual llegaron.
        raw: Params,
    },
}

impl TuiPanel {
    /// El listado, si este panel es uno.
    #[must_use]
    pub fn as_browser(&self) -> Option<&Pane> {
        match self {
            Self::Browser(p) => Some(p),
            Self::Unknown { .. } => None,
        }
    }

    /// El listado, para mutarlo.
    pub fn as_browser_mut(&mut self) -> Option<&mut Pane> {
        match self {
            Self::Browser(p) => Some(p),
            Self::Unknown { .. } => None,
        }
    }
}

/// Los dos listados del preset `orthodox`, guardados por hueco.
///
/// Se indexa por LADO (`0` izquierda, `1` derecha) y por dentro traduce a
/// [`SLOT_LEFT`]/[`SLOT_RIGHT`]. Ver el módulo para por qué.
#[derive(Debug)]
pub struct PaneSlots {
    store: SlotStore<TuiPanel>,
}

impl PaneSlots {
    /// Los dos listados de arranque.
    #[must_use]
    pub fn new(left: Pane, right: Pane) -> Self {
        let mut store = SlotStore::default();
        store.insert(SLOT_LEFT, TuiPanel::Browser(Box::new(left)));
        store.insert(SLOT_RIGHT, TuiPanel::Browser(Box::new(right)));
        Self { store }
    }

    /// El hueco de un lado.
    #[must_use]
    pub const fn slot_of(side: usize) -> SlotId {
        if side == 0 { SLOT_LEFT } else { SLOT_RIGHT }
    }

    /// Cuántos listados hay. Dos en L1a, por construcción.
    #[must_use]
    pub fn len(&self) -> usize {
        2
    }

    /// Nunca. Existe porque clippy lo pide junto a [`Self::len`].
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// El listado de un lado, o `None` si el índice no es `0|1`.
    ///
    /// Existe porque hay un llamante —el veredicto de solo-lectura— cuyo
    /// índice puede venir de fuera y para el que un índice imposible NO debe
    /// ser un panic, sino un «no sé».
    #[must_use]
    pub fn get(&self, side: usize) -> Option<&Pane> {
        if side > 1 {
            return None;
        }
        self.store
            .get(Self::slot_of(side))
            .and_then(TuiPanel::as_browser)
    }

    /// Los listados, de izquierda a derecha.
    pub fn iter(&self) -> impl Iterator<Item = &Pane> {
        self.store.values().filter_map(TuiPanel::as_browser)
    }

    /// Los listados, para mutarlos.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Pane> {
        self.store.values_mut().filter_map(TuiPanel::as_browser_mut)
    }

    /// Intercambia el contenido de los dos lados, dejando los ids quietos.
    pub fn swap(&mut self, a: usize, b: usize) {
        self.store.swap(Self::slot_of(a), Self::slot_of(b));
    }

    /// El store de verdad, para quien ya piensa en huecos.
    #[must_use]
    pub const fn store(&self) -> &SlotStore<TuiPanel> {
        &self.store
    }

    /// El store de verdad, para mutarlo.
    pub const fn store_mut(&mut self) -> &mut SlotStore<TuiPanel> {
        &mut self.store
    }
}

impl Index<usize> for PaneSlots {
    type Output = Pane;

    /// # Panics
    ///
    /// Si el lado no lleva un listado. En L1a no puede pasar: `PaneSlots::new`
    /// pone los dos y nada los quita — el único camino que cambia el contenido
    /// es [`PaneSlots::swap`], que los intercambia entre sí.
    fn index(&self, side: usize) -> &Self::Output {
        self.store
            .get(Self::slot_of(side))
            .and_then(TuiPanel::as_browser)
            .expect("el preset orthodox siempre lleva sus dos listados")
    }
}

impl IndexMut<usize> for PaneSlots {
    /// # Panics
    ///
    /// Igual que [`Index::index`].
    fn index_mut(&mut self, side: usize) -> &mut Self::Output {
        self.store
            .get_mut(Self::slot_of(side))
            .and_then(TuiPanel::as_browser_mut)
            .expect("el preset orthodox siempre lleva sus dos listados")
    }
}

impl<'a> IntoIterator for &'a PaneSlots {
    type Item = &'a Pane;
    type IntoIter = Box<dyn Iterator<Item = &'a Pane> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl<'a> IntoIterator for &'a mut PaneSlots {
    type Item = &'a mut Pane;
    type IntoIter = Box<dyn Iterator<Item = &'a mut Pane> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter_mut())
    }
}

/// El preset por defecto: el FRAME entero, tal como se ve hoy.
///
/// ```text
/// Split V   [Weight(1), Auto, Fixed(1)]
///   ├── Split H  [Weight(1), Weight(1)]  →  browser, browser
///   ├── tasks     ← Auto: mide lo que pidan las tareas, CERO en reposo
///   └── status    ← Fixed(1)
/// ```
///
/// El `Auto` de la franja de tareas es la razón de que exista [`Size::Auto`]:
/// hoy vale cero con el sistema en reposo, así que un `Fixed(6)` pintaría seis
/// filas vacías donde ahora no hay nada. Lo sustituye el frontend con
/// [`Node::substitute_auto`] antes de repartir, porque el único que sabe
/// cuántas tareas hay es quien tiene el `TaskBoard` delante.
#[must_use]
pub fn orthodox() -> Node {
    Node::Split {
        dir: Dir::Vertical,
        sizes: vec![Size::Weight(1), Size::Auto, Size::Fixed(1)],
        children: vec![
            Node::split(
                Dir::Horizontal,
                vec![
                    Node::slot(SLOT_LEFT, KindId::browser()),
                    Node::slot(SLOT_RIGHT, KindId::browser()),
                ],
            ),
            Node::slot(SLOT_TASKS, KindId::new("tasks")),
            Node::slot(SLOT_STATUS, KindId::new("status")),
        ],
    }
}

/// Celdas a `ratatui::layout::Rect`, campo a campo. Los nombres coinciden a
/// propósito: aquí no hay interpretación que hacer.
#[must_use]
pub const fn to_ratatui(r: LayoutRect) -> ratatui::layout::Rect {
    ratatui::layout::Rect {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    }
}

/// Y de vuelta.
#[must_use]
pub const fn from_ratatui(r: ratatui::layout::Rect) -> LayoutRect {
    LayoutRect {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::VPath;

    fn pane(wire: &str) -> Pane {
        Pane::new(VPath::parse(wire).expect("wire"), Vec::new())
    }

    /// Indexar por lado da el mismo listado que antes daba el array.
    #[test]
    fn los_dos_lados_se_indexan_como_el_array_de_antes() {
        let slots = PaneSlots::new(pane("mem:///izq"), pane("mem:///der"));
        assert_eq!(slots[0].dir(), &VPath::parse("mem:///izq").expect("wire"));
        assert_eq!(slots[1].dir(), &VPath::parse("mem:///der").expect("wire"));
        assert_eq!(slots.len(), 2);
    }

    /// Iterar los recorre de izquierda a derecha. El orden importa: el render
    /// pinta por índice y un orden invertido cambiaría la pantalla entera.
    #[test]
    fn iterar_va_de_izquierda_a_derecha() {
        let slots = PaneSlots::new(pane("mem:///izq"), pane("mem:///der"));
        let dirs: Vec<String> = slots.iter().map(|p| p.dir().to_wire()).collect();
        assert_eq!(dirs, vec!["mem:///izq".to_owned(), "mem:///der".to_owned()]);
    }

    /// El preset lleva los dos huecos bien conocidos, y solo esos: si trajera
    /// otro id, el estado de arranque y el layout dejarían de ser la misma
    /// cosa y habría que migrar algo que nunca hizo falta migrar.
    #[test]
    fn el_preset_orthodox_lleva_los_dos_huecos_de_siempre() {
        assert_eq!(
            orthodox().slot_ids(),
            vec![SLOT_LEFT, SLOT_RIGHT, SLOT_TASKS, SLOT_STATUS]
        );
    }

    /// La conversión de rectángulos es campo a campo en los dos sentidos.
    #[test]
    fn los_rectangulos_van_y_vuelven_iguales() {
        let r = LayoutRect::new(3, 4, 50, 20);
        assert_eq!(from_ratatui(to_ratatui(r)), r);
    }

    /// Intercambiar mueve el contenido y NO los huecos: lo que guarda un
    /// `SlotId` de antes sigue nombrando al mismo sitio de la pantalla.
    #[test]
    fn intercambiar_mueve_el_contenido_y_deja_los_huecos() {
        let mut slots = PaneSlots::new(pane("mem:///izq"), pane("mem:///der"));
        slots.swap(0, 1);
        assert_eq!(slots[0].dir(), &VPath::parse("mem:///der").expect("wire"));
        assert_eq!(slots[1].dir(), &VPath::parse("mem:///izq").expect("wire"));
        assert!(
            slots.store().get(SLOT_LEFT).is_some(),
            "el hueco izquierdo sigue existiendo"
        );
    }
}
