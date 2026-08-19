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

use norte_frontend::layout::{BySlot, KindId, Node, Params, Rect as LayoutRect, SlotId, SlotStore};

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
    /// El visor ACOPLADO (L3): el kind `viewer` en un hueco, siguiendo al
    /// listado activo. El visor a pantalla completa sigue siendo `App::viewer`
    /// y no pasa por aquí.
    Preview(Box<crate::preview::Preview>),
    /// El sidebar de sitios (L3): discos y favoritos.
    ///
    /// El primer panel que no es un listado. Que `as_browser` devuelva `None`
    /// para él es lo que mantiene `app.panes[i]` queriendo decir «el i-ésimo
    /// LISTADO»: un sidebar no es un lado.
    Places(Box<norte_frontend::places::PlacesState>),
    /// El panel de procesos (fase A): su cursor. Las filas son del
    /// `TaskBoard`, que es de `App`: aquí no hay una segunda copia.
    Processes(Box<crate::processes::Processes>),
    /// El árbol de directorios (#136): sus ramas abiertas y su cursor.
    Tree(Box<crate::tree::Tree>),
    /// La hoja de atributos (fase A): la entrada que se está enseñando.
    ///
    /// Guarda la `Entry` y no su ruta: la hoja se dibuja entera desde ella y
    /// no hay una segunda lectura que pueda llegar tarde.
    Metadata(Box<Option<norte_proto::Entry>>),
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
            Self::Places(_)
            | Self::Preview(_)
            | Self::Processes(_)
            | Self::Tree(_)
            | Self::Metadata(_)
            | Self::Unknown { .. } => None,
        }
    }

    /// El listado, para mutarlo.
    pub fn as_browser_mut(&mut self) -> Option<&mut Pane> {
        match self {
            Self::Browser(p) => Some(p),
            Self::Places(_)
            | Self::Preview(_)
            | Self::Processes(_)
            | Self::Tree(_)
            | Self::Metadata(_)
            | Self::Unknown { .. } => None,
        }
    }

    /// El sidebar, si este panel es uno.
    #[must_use]
    pub fn as_places(&self) -> Option<&norte_frontend::places::PlacesState> {
        match self {
            Self::Places(s) => Some(s),
            Self::Browser(_)
            | Self::Preview(_)
            | Self::Processes(_)
            | Self::Tree(_)
            | Self::Metadata(_)
            | Self::Unknown { .. } => None,
        }
    }

    /// El sidebar, para mutarlo.
    pub fn as_places_mut(&mut self) -> Option<&mut norte_frontend::places::PlacesState> {
        match self {
            Self::Places(s) => Some(s),
            Self::Browser(_)
            | Self::Preview(_)
            | Self::Processes(_)
            | Self::Tree(_)
            | Self::Metadata(_)
            | Self::Unknown { .. } => None,
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
    /// Qué hueco enseña cada POSICIÓN visible, de izquierda a derecha.
    ///
    /// Con pestañas hay más `browser` vivos que visibles, y con splits hay más
    /// de dos visibles. «El pane izquierdo» sigue queriendo decir lo mismo que
    /// siempre —el listado pintado más a la izquierda—, así que los ~212 sitios
    /// que dicen `app.panes[0]` valen igual. Lo pone al día
    /// [`Self::set_visible`] tras cada reparto.
    visible: Vec<SlotId>,
}

impl PaneSlots {
    /// Los dos listados de arranque.
    #[must_use]
    pub fn new(left: Pane, right: Pane) -> Self {
        let mut store = SlotStore::default();
        store.insert(SLOT_LEFT, TuiPanel::Browser(Box::new(left)));
        store.insert(SLOT_RIGHT, TuiPanel::Browser(Box::new(right)));
        Self {
            store,
            visible: vec![SLOT_LEFT, SLOT_RIGHT],
        }
    }

    /// El hueco que enseña una posición. Fuera de rango, la última.
    #[must_use]
    pub fn slot_of(&self, side: usize) -> SlotId {
        let i = side.min(self.visible.len().saturating_sub(1));
        self.visible.get(i).copied().unwrap_or(SLOT_LEFT)
    }

    /// Dice qué hueco enseña cada posición. Lo llama el frontend tras repartir,
    /// con los `browser` colocados ordenados de izquierda a derecha.
    ///
    /// Una lista VACÍA no borra nada: pasa cuando el reparto no coloca ningún
    /// pane (visor abierto, o una ventana imposible), y en ese frame lo que
    /// había sigue siendo lo correcto.
    pub fn set_visible(&mut self, orden: &[SlotId]) {
        if !orden.is_empty() {
            self.visible = orden.to_vec();
        }
        self.rescue_visible();
    }

    /// Echa de `visible` los huecos que ya NO llevan listado, y si con eso se
    /// queda sin ninguno coge cualquier listado del store.
    ///
    /// Es la red del invariante que documentan [`Index`] e [`IndexMut`]: sin
    /// ella, un hueco cuyo contenido pasó a ser otro kind se quedaba en la
    /// lista —«vacía no borra nada» conserva lo ANTERIOR, no lo válido— y el
    /// primer acceso por lado panicaba (#242). Que el árbol traiga un listado
    /// lo garantiza [`norte_frontend::layout::validate`]; que la lista apunte
    /// a uno, esto.
    fn rescue_visible(&mut self) {
        self.visible
            .retain(|id| matches!(self.store.get(*id), Some(TuiPanel::Browser(_))));
        if self.visible.is_empty()
            && let Some(id) = self
                .store
                .iter()
                .find(|(_, p)| matches!(p, TuiPanel::Browser(_)))
                .map(|(id, _)| id)
        {
            self.visible = vec![id];
        }
    }

    /// Pone al día los lados a partir del ÁRBOL, sin repartir nada.
    ///
    /// Se llama justo después de tocar el layout, cuando todavía no hay frame:
    /// sin esto, un lado apuntaría al hueco que se acaba de cerrar y el
    /// siguiente `app.panes[i]` reventaría.
    ///
    /// Con un solo `browser` vivo la lista tiene UNA entrada, y `len()` lo
    /// dice: quien pregunte por «el otro pane» recibe ese mismo, que es la
    /// verdad — no hay otro.
    pub fn refresh_visible(&mut self, tree: &Node) {
        let vivos: Vec<SlotId> = tree
            .visible_slot_ids()
            .into_iter()
            .filter(|id| {
                tree.kind_of(*id).is_some_and(|k| *k == KindId::browser())
                    && matches!(self.store.get(*id), Some(TuiPanel::Browser(_)))
            })
            .collect();
        if !vivos.is_empty() {
            self.visible = vivos;
        }
        self.store.sync_with(tree);
        self.rescue_visible();
    }

    /// El listado de un hueco cualquiera, visible o no. Lo pide la barra de
    /// pestañas, que tiene que titular también las que no se ven.
    #[must_use]
    pub fn browser(&self, id: SlotId) -> Option<&Pane> {
        self.store.get(id).and_then(TuiPanel::as_browser)
    }

    /// El listado de un hueco cualquiera, para mutarlo.
    ///
    /// Es la puerta que necesita el bucle: una respuesta en vuelo lleva el
    /// HUECO al que iba, y aplicarla por posición sería aplicarla a quien
    /// ocupe esa posición cuando llegue.
    pub fn browser_mut(&mut self, id: SlotId) -> Option<&mut Pane> {
        self.store.get_mut(id).and_then(TuiPanel::as_browser_mut)
    }

    /// Mete un listado nuevo en el store, para un hueco recién acuñado.
    pub fn insert_browser(&mut self, id: SlotId, pane: Pane) {
        self.store.insert(id, TuiPanel::Browser(Box::new(pane)));
    }

    /// Cuántos listados hay VISIBLES.
    #[must_use]
    pub fn len(&self) -> usize {
        self.visible.len()
    }

    /// Nunca: siempre hay al menos un listado en pantalla.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
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
            .get(self.slot_of(side))
            .and_then(TuiPanel::as_browser)
    }

    /// Los huecos visibles, sin repetir.
    fn visibles(&self) -> Vec<SlotId> {
        let mut v = self.visible.clone();
        v.dedup();
        v
    }

    /// Los listados VISIBLES, de izquierda a derecha.
    ///
    /// Los de las pestañas ocultas NO salen: quien itera los panes está
    /// haciendo algo con lo que hay en pantalla —vigilar su directorio, pedir
    /// una página, refrescar—, y una pestaña que nadie mira no debe costar
    /// nada. La suspensión de un hueco oculto no es código aparte: es esto.
    pub fn iter(&self) -> impl Iterator<Item = &Pane> {
        self.visibles()
            .into_iter()
            .filter_map(|id| self.store.get(id).and_then(TuiPanel::as_browser))
    }

    /// Como [`Self::iter`], para mutarlos.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Pane> {
        let ids = self.visibles();
        self.store
            .iter_mut()
            .filter(move |(id, _)| ids.contains(id))
            .filter_map(|(_, p)| p.as_browser_mut())
    }

    /// Intercambia el contenido de los dos lados, dejando los ids quietos.
    pub fn swap(&mut self, a: usize, b: usize) {
        let (sa, sb) = (self.slot_of(a), self.slot_of(b));
        self.store.swap(sa, sb);
    }

    /// El sidebar de un hueco, si lo hay.
    #[must_use]
    pub fn places(&self, id: SlotId) -> Option<&norte_frontend::places::PlacesState> {
        self.store.get(id).and_then(TuiPanel::as_places)
    }

    /// El sidebar de un hueco, para mutarlo.
    pub fn places_mut(&mut self, id: SlotId) -> Option<&mut norte_frontend::places::PlacesState> {
        self.store.get_mut(id).and_then(TuiPanel::as_places_mut)
    }

    /// El preview de un hueco, si lo hay.
    #[must_use]
    pub fn preview(&self, id: SlotId) -> Option<&crate::preview::Preview> {
        match self.store.get(id) {
            Some(TuiPanel::Preview(p)) => Some(p),
            _ => None,
        }
    }

    /// El preview de un hueco, para mutarlo.
    pub fn preview_mut(&mut self, id: SlotId) -> Option<&mut crate::preview::Preview> {
        match self.store.get_mut(id) {
            Some(TuiPanel::Preview(p)) => Some(p),
            _ => None,
        }
    }

    /// Mete un preview nuevo en el store, para un hueco recién acuñado.
    pub fn insert_preview(&mut self, id: SlotId, p: crate::preview::Preview) {
        self.store.insert(id, TuiPanel::Preview(Box::new(p)));
    }

    /// El panel de procesos de un hueco, si lo hay.
    #[must_use]
    pub fn processes(&self, id: SlotId) -> Option<&crate::processes::Processes> {
        match self.store.get(id) {
            Some(TuiPanel::Processes(p)) => Some(p),
            _ => None,
        }
    }

    /// El panel de procesos de un hueco, para mover su cursor.
    pub fn processes_mut(&mut self, id: SlotId) -> Option<&mut crate::processes::Processes> {
        match self.store.get_mut(id) {
            Some(TuiPanel::Processes(p)) => Some(p),
            _ => None,
        }
    }

    /// Mete un panel de procesos nuevo, para un hueco recién acuñado.
    /// El árbol de ese hueco, si lo es.
    #[must_use]
    pub fn tree(&self, id: SlotId) -> Option<&crate::tree::Tree> {
        match self.store.get(id) {
            Some(TuiPanel::Tree(t)) => Some(t),
            _ => None,
        }
    }

    /// El árbol de ese hueco, para mutarlo.
    pub fn tree_mut(&mut self, id: SlotId) -> Option<&mut crate::tree::Tree> {
        match self.store.get_mut(id) {
            Some(TuiPanel::Tree(t)) => Some(t),
            _ => None,
        }
    }

    /// Mete un árbol en un hueco.
    pub fn insert_tree(&mut self, id: SlotId, t: crate::tree::Tree) {
        self.store.insert(id, TuiPanel::Tree(Box::new(t)));
    }

    /// Mete el panel de procesos en un hueco.
    pub fn insert_processes(&mut self, id: SlotId, p: crate::processes::Processes) {
        self.store.insert(id, TuiPanel::Processes(Box::new(p)));
    }

    /// Lo que enseña la hoja de atributos de un hueco, si lo hay.
    #[must_use]
    pub fn metadata(&self, id: SlotId) -> Option<&Option<norte_proto::Entry>> {
        match self.store.get(id) {
            Some(TuiPanel::Metadata(e)) => Some(e),
            _ => None,
        }
    }

    /// La hoja de atributos de un hueco, para ponerla al día.
    pub fn metadata_mut(&mut self, id: SlotId) -> Option<&mut Option<norte_proto::Entry>> {
        match self.store.get_mut(id) {
            Some(TuiPanel::Metadata(e)) => Some(e),
            _ => None,
        }
    }

    /// Mete una hoja de atributos nueva, para un hueco recién acuñado.
    pub fn insert_metadata(&mut self, id: SlotId, e: Option<norte_proto::Entry>) {
        self.store.insert(id, TuiPanel::Metadata(Box::new(e)));
    }

    /// Mete un sidebar nuevo en el store, para un hueco recién acuñado.
    pub fn insert_places(&mut self, id: SlotId, state: norte_frontend::places::PlacesState) {
        self.store.insert(id, TuiPanel::Places(Box::new(state)));
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
            .get(self.slot_of(side))
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
            .get_mut(self.slot_of(side))
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

/// Los historiales de navegación, indexados por HUECO y accedidos por
/// posición.
///
/// Estaban en un `[History; 2]` porque había exactamente dos panes. Con
/// pestañas y splits, un historial pertenece a su listado, no al sitio de la
/// pantalla donde se pinta hoy: cambiar de pestaña y encontrarse el historial
/// de la otra sería el mismo bug que ver su cursor.
///
/// El orden lo pone [`Self::set_order`], en la MISMA función que
/// [`PaneSlots::set_visible`] y con el mismo valor — están juntas a propósito,
/// porque dos listas de orden que se puedan desincronizar son un fallo que solo
/// se ve al cambiar de pestaña.
#[derive(Debug, Default)]
pub struct Histories {
    por_hueco: BySlot<crate::nav::History>,
    orden: Vec<SlotId>,
}

impl Histories {
    /// Los dos de arranque.
    #[must_use]
    pub fn new() -> Self {
        Self {
            por_hueco: BySlot::new(),
            orden: vec![SLOT_LEFT, SLOT_RIGHT],
        }
    }

    /// Dice qué hueco ocupa cada posición visible.
    pub fn set_order(&mut self, orden: &[SlotId]) {
        if !orden.is_empty() {
            self.orden = orden.to_vec();
        }
    }

    /// El hueco de una posición.
    fn slot_of(&self, side: usize) -> SlotId {
        let i = side.min(self.orden.len().saturating_sub(1));
        self.orden.get(i).copied().unwrap_or(SLOT_LEFT)
    }

    /// Intercambia el historial de dos posiciones, para el gesto de
    /// intercambiar paneles.
    pub fn swap(&mut self, a: usize, b: usize) {
        let (sa, sb) = (self.slot_of(a), self.slot_of(b));
        if sa == sb {
            return;
        }
        let (va, vb) = (self.por_hueco.remove(sa), self.por_hueco.remove(sb));
        if let Some(v) = vb {
            self.por_hueco.insert(sa, v);
        }
        if let Some(v) = va {
            self.por_hueco.insert(sb, v);
        }
    }

    /// El historial de UN hueco, por su id.
    #[must_use]
    pub fn for_slot(&self, id: SlotId) -> Option<&crate::nav::History> {
        self.por_hueco.get(id)
    }

    /// El historial de UN hueco, creándolo vacío si no lo tenía.
    pub fn for_slot_mut(&mut self, id: SlotId) -> &mut crate::nav::History {
        self.por_hueco.entry(id)
    }

    /// Tira los historiales de los huecos que el árbol ya no tiene.
    pub fn retain_tree(&mut self, tree: &Node) {
        self.por_hueco.retain_tree(tree);
    }
}

impl std::ops::Index<usize> for Histories {
    type Output = crate::nav::History;

    fn index(&self, side: usize) -> &Self::Output {
        // Un hueco sin historial todavía es un hueco recién abierto: se le
        // devuelve uno vacío, que es exactamente su historia.
        static VACIO: std::sync::OnceLock<crate::nav::History> = std::sync::OnceLock::new();
        self.por_hueco
            .get(self.slot_of(side))
            .unwrap_or_else(|| VACIO.get_or_init(crate::nav::History::default))
    }
}

impl std::ops::IndexMut<usize> for Histories {
    fn index_mut(&mut self, side: usize) -> &mut Self::Output {
        let id = self.slot_of(side);
        self.por_hueco.entry(id)
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
/// El `Auto` de la franja de tareas es la razón de que exista
/// [`Size::Auto`](norte_frontend::layout::Size::Auto):
/// hoy vale cero con el sistema en reposo, así que un `Fixed(6)` pintaría seis
/// filas vacías donde ahora no hay nada. Lo sustituye el frontend con
/// [`Node::substitute_auto`] antes de repartir, porque el único que sabe
/// cuántas tareas hay es quien tiene el `TaskBoard` delante.
///
/// Sale del PRESET de fábrica, no de un árbol escrito aquí: dos definiciones
/// de la misma pantalla se separan, y la que se cargue de un fichero ganaría
/// sin que nadie lo note. El test de este módulo fija que son iguales.
#[must_use]
pub fn orthodox() -> Node {
    // El `unwrap` está justificado por los tests de `layout::presets`, que
    // parsean y validan los cinco presets en cada CI: si este fallara, el
    // binario se envió con un fichero embebido que no compila como árbol.
    norte_frontend::layout::presets::tree("orthodox")
        .unwrap_or_else(|e| unreachable!("el preset de fábrica no parsea: {e}"))
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
    use norte_frontend::layout::Dir;
    use norte_proto::VPath;

    fn pane(wire: &str) -> Pane {
        Pane::new(VPath::parse(wire).expect("wire"), Vec::new())
    }

    /// Los cuatro huecos que el TUI nombra son los cuatro que trae el fichero.
    ///
    /// `orthodox()` ya no construye el árbol, lo lee del preset de fábrica, y
    /// una igualdad contra sí mismo no probaría nada. Lo que hay que fijar es
    /// lo otro: que `SLOT_LEFT` y sus tres compañeros siguen queriendo decir
    /// en el fichero lo que quieren decir en el código. Renumerar el fichero
    /// dejaría a los ~212 sitios que dicen `app.panes[0]` apuntando a un hueco
    /// que no es un listado.
    #[test]
    fn los_huecos_con_nombre_son_los_del_fichero() {
        let arbol = orthodox();
        assert_eq!(
            arbol.slot_ids(),
            vec![SLOT_LEFT, SLOT_RIGHT, SLOT_TASKS, SLOT_STATUS]
        );
        let kind = |id| arbol.kind_of(id).expect("kind").as_str().to_owned();
        assert_eq!(kind(SLOT_LEFT), "browser");
        assert_eq!(kind(SLOT_RIGHT), "browser");
        assert_eq!(kind(SLOT_TASKS), "tasks");
        assert_eq!(kind(SLOT_STATUS), "status");
    }

    /// Un árbol que llama `places` al hueco donde estaba el listado dejaba
    /// `visible` apuntando a un hueco que YA no lleva uno —la lista vacía
    /// «conserva lo anterior»— y el primer `panes[0]` panicaba (#242). Con la
    /// validación puesta ese árbol ya no llega, pero la lista tiene que ser
    /// coherente por sí sola: es lo que documenta el `expect` de `Index`.
    #[test]
    fn un_lado_nunca_apunta_a_un_hueco_sin_listado() {
        let mut slots = PaneSlots::new(pane("mem:///izq"), pane("mem:///der"));
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SLOT_LEFT, KindId::new("places")),
                Node::slot(SlotId(5), KindId::browser()),
            ],
        );
        slots.insert_places(SLOT_LEFT, norte_frontend::places::PlacesState::new());
        slots.insert_browser(SlotId(5), pane("mem:///cinco"));
        slots.refresh_visible(&arbol);
        assert_eq!(slots.slot_of(0), SlotId(5), "el lado va al listado que hay");
        assert_eq!(slots[0].dir(), &VPath::parse("mem:///cinco").expect("wire"));
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
