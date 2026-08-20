//! Los panes vistos desde `App`: cuál tiene el foco, cómo se cambia y se
//! intercambia, qué ventana de entradas necesita `stat`, y las pestañas de
//! un hueco (abrir, cerrar, ciclar, ir a una y moverla).

use super::App;
use super::pane::Pane;
use norte_proto::{EntryKind, VPath};

impl App {
    /// Índice del pane con foco (0 = izquierda, 1 = derecha).
    #[must_use]
    pub fn focus(&self) -> usize {
        self.focus
    }

    /// El pane con foco.
    #[must_use]
    pub fn focused(&self) -> &Pane {
        &self.panes[self.focus]
    }

    /// (índice, path) de la entrada File enfocada sin `size`: candidata a la
    /// sonda de stat on-focus (#52, listado lazy).
    #[must_use]
    pub fn focused_needs_stat(&self) -> Option<(usize, VPath)> {
        let e = self.focused().selected()?;
        (e.kind == EntryKind::File && e.size.is_none()).then(|| (self.focus(), e.path.clone()))
    }

    /// Candidatas a hidratar de la VENTANA visible (#52, listado lazy) en
    /// LOS DOS panes — ambos se pintan a la vez, así que sondear solo la
    /// entrada enfocada dejaba las columnas Tamaño/Fecha en blanco en todo
    /// lo demás. El pane con foco va primero; la selección DENTRO de cada
    /// pane es del modelo compartido
    /// ([`norte_frontend::PaneState::needs_stat_window`], regla 7).
    #[must_use]
    pub fn needs_stat_window(&self, radius: usize) -> Vec<(usize, VPath)> {
        let mut out = Vec::new();
        for pane_idx in [self.focus(), self.focus() ^ 1] {
            out.extend(
                self.panes[pane_idx]
                    .needs_stat_window(radius)
                    .into_iter()
                    .map(|p| (pane_idx, p)),
            );
        }
        out
    }

    /// El pane con foco, mutable.
    pub fn focused_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.focus]
    }

    /// Alterna el foco entre los dos panes (Tab, keymap mc).
    pub fn switch_focus(&mut self) {
        self.focus ^= 1;
    }

    /// Exchanges the two panes and everything `App` keeps beside them
    /// (`pane.swap`).
    ///
    /// Touches no disk: no listing is refetched, nothing can fail, and the
    /// marks, the filter, the sort and the cursor all survive because the
    /// WHOLE pane moves rather than being rebuilt.
    ///
    /// The focus stays on the same physical SIDE on purpose. Moving it along
    /// with the content would make the command a no-op from where the reader
    /// sits: they would still be looking at the same listing, just on the
    /// other half of the screen.
    ///
    /// The history moves WITH the pane, because it belongs to the content and
    /// not to the side of the screen. Left behind, each pane would offer to
    /// take the reader "back" to places that content has never been.
    ///
    /// NOT the whole story: the run loop keeps its own state indexed by pane
    /// (the paginated fills in flight, the decoration fetches, the stat-probe
    /// dedup, the live search run) which `App` cannot see.
    /// `main::reconcile_swap` is the other half, and the two are driven
    /// together by `Cd::Swapped`.
    pub fn swap_panes(&mut self) {
        self.panes.swap(0, 1);
        self.history.swap(0, 1);
        // Lo ÚNICO que queda como rastro de que hubo intercambio: todo lo
        // demás viaja con su pane, así que quien compare por lado no ve
        // moverse nada (ver [`Self::swap_seq`]).
        self.swap_seq = self.swap_seq.wrapping_add(1);
    }

    /// Acuña un `SlotId` que no se ha usado nunca en esta sesión.
    pub(super) fn mint_slot(&mut self) -> norte_frontend::layout::SlotId {
        let id = norte_frontend::layout::SlotId(self.next_slot);
        self.next_slot = self.next_slot.saturating_add(1);
        id
    }

    /// El hueco que el lado enfocado enseña ahora.
    #[must_use]
    pub fn focused_slot(&self) -> norte_frontend::layout::SlotId {
        self.panes.slot_of(self.focus)
    }

    /// Abre una pestaña nueva junto al pane enfocado, en el mismo directorio.
    ///
    /// Hereda las entradas ya listadas en vez de pedir un listado: es el MISMO
    /// directorio que se está mirando, así que la pestaña aparece llena en el
    /// acto y no parpadea vacía mientras alguien vuelve a leer lo mismo.
    pub fn tab_new(&mut self) {
        let focus = self.focused_slot();
        let (dir, entradas) = {
            let p = &self.panes[self.focus];
            (p.dir().clone(), p.entries().to_vec())
        };
        let id = self.mint_slot();
        self.panes.insert_browser(id, Pane::new(dir, entradas));
        self.layout = self.layout.add_tab(
            focus,
            &norte_frontend::layout::Node::slot(id, norte_frontend::layout::KindId::browser()),
        );
        self.panes.refresh_visible(&self.layout);
        self.history.retain_tree(&self.layout);
    }

    /// Cierra la pestaña enfocada. Sin efecto si el pane no está en un grupo.
    pub fn tab_close(&mut self) {
        let focus = self.focused_slot();
        if let Some(nuevo) = self.layout.close_tab(focus) {
            self.layout = nuevo;
            self.panes.refresh_visible(&self.layout);
            self.history.retain_tree(&self.layout);
        }
    }

    /// Cambia de pestaña dentro del grupo enfocado, ciclando.
    pub fn tab_cycle(&mut self, delta: isize) {
        let focus = self.focused_slot();
        let Some((tabs, active)) = self.layout.tabs_of(focus) else {
            return;
        };
        if tabs.is_empty() {
            return;
        }
        let n = isize::try_from(tabs.len()).unwrap_or(1);
        let i = isize::try_from(active).unwrap_or(0);
        let dest = usize::try_from((i + delta).rem_euclid(n)).unwrap_or(0);
        self.layout = self.layout.set_active_for(focus, dest);
        self.panes.refresh_visible(&self.layout);
        self.history.retain_tree(&self.layout);
    }

    /// Va a la pestaña `n` (base 1) del grupo enfocado.
    pub fn tab_goto(&mut self, n: usize) {
        let focus = self.focused_slot();
        if self.layout.tabs_of(focus).is_some() {
            self.layout = self.layout.set_active_for(focus, n.saturating_sub(1));
            self.panes.refresh_visible(&self.layout);
            self.history.retain_tree(&self.layout);
        }
    }

    /// Mueve la pestaña enfocada dentro de su grupo. No da la vuelta: una
    /// pestaña que salta del final al principio por una pulsación de más es
    /// justo lo que nadie quería.
    pub fn tab_move(&mut self, delta: isize) {
        let focus = self.focused_slot();
        if self.layout.tabs_of(focus).is_some() {
            self.layout = self.layout.move_tab(focus, delta);
            self.panes.refresh_visible(&self.layout);
            self.history.retain_tree(&self.layout);
        }
    }

    /// Cuántos `browser` hay en el árbol, visibles u ocultos.
    pub(super) fn browsers_in_tree(&self) -> usize {
        self.layout
            .slot_ids()
            .into_iter()
            .filter(|id| {
                self.layout
                    .kind_of(*id)
                    .is_some_and(|k| *k == norte_frontend::layout::KindId::browser())
            })
            .count()
    }

    /// Pasa el foco al siguiente lado visible.
    pub fn layout_focus(&mut self, delta: isize) {
        let n = isize::try_from(self.panes.len()).unwrap_or(2);
        let i = isize::try_from(self.focus).unwrap_or(0);
        self.set_focus(usize::try_from((i + delta).rem_euclid(n)).unwrap_or(0));
    }
}
