//! El estado de los paneles, indexado por hueco.

use std::collections::BTreeMap;

use super::{Node, SlotId};

/// Cuántos estados huérfanos se conservan antes de purgar el más antiguo.
///
/// Generoso para una sesión de trabajo y acotado para que abrir y cerrar
/// paneles toda una tarde no crezca sin fin.
const ORPHAN_CAP: usize = 32;

/// El estado de los paneles, indexado por hueco.
///
/// GENÉRICO sobre el estado, y cada frontend pone el suyo: este crate tiene
/// las mitades puras (`pane`, `viewer`, `compare`, `sync`), pero el TUI las
/// envuelve en vistas propias y la GUI en otras distintas. Un enum concreto
/// aquí arrastraría los tipos de vista de un frontend al grafo del otro.
///
/// La elegibilidad para un rol NO necesita un trait sobre `P`: la decide el
/// `kind` del hueco en el árbol más el registro.
///
/// # Los huérfanos, y por qué no se borran
///
/// Cerrar un hueco no borra su estado: pasa a huérfano. Reabrir la misma
/// disposición recupera el historial, las marcas y el cursor en vez de
/// arrancar en blanco — que es lo que un usuario espera de cerrar una pestaña
/// por error. El precio es acotado: pasado el tope (32 por defecto) se purga el más
/// antiguo, y la edad es el orden en que quedaron huérfanos, no el reloj (aquí
/// no hay reloj, y no se quiere uno: haría los tests dependientes del tiempo).
#[derive(Debug, Clone)]
pub struct SlotStore<P> {
    slots: BTreeMap<SlotId, P>,
    /// Huérfanos del más ANTIGUO al más reciente.
    orphans: Vec<SlotId>,
    cap: usize,
}

impl<P> Default for SlotStore<P> {
    fn default() -> Self {
        Self::with_orphan_cap(ORPHAN_CAP)
    }
}

impl<P> SlotStore<P> {
    /// Un store con un tope de huérfanos a medida.
    #[must_use]
    pub fn with_orphan_cap(cap: usize) -> Self {
        Self {
            slots: BTreeMap::new(),
            orphans: Vec::new(),
            cap,
        }
    }

    /// Pone el estado de un hueco. Si ese id estaba huérfano, revive.
    pub fn insert(&mut self, id: SlotId, state: P) {
        self.orphans.retain(|o| *o != id);
        self.slots.insert(id, state);
    }

    /// El estado del hueco `id`, vivo o huérfano.
    #[must_use]
    pub fn get(&self, id: SlotId) -> Option<&P> {
        self.slots.get(&id)
    }

    /// El estado del hueco `id`, para mutarlo.
    pub fn get_mut(&mut self, id: SlotId) -> Option<&mut P> {
        self.slots.get_mut(&id)
    }

    /// Quita un hueco y su estado del todo. Lo usa quien de verdad quiere
    /// olvidar, no el cierre de un panel.
    pub fn remove(&mut self, id: SlotId) -> Option<P> {
        self.orphans.retain(|o| *o != id);
        self.slots.remove(&id)
    }

    /// Reclasifica contra `tree`: lo que el árbol menciona está VIVO, lo demás
    /// queda huérfano; pasado el tope, se purga el más antiguo.
    ///
    /// Se llama tras cada cambio de layout.
    pub fn sync_with(&mut self, tree: &Node) {
        let vivos = tree.slot_ids();
        // Los que vuelven al árbol dejan de ser huérfanos.
        self.orphans.retain(|o| !vivos.contains(o));
        // Los que salieron del árbol y aún no estaban en la lista, entran.
        for id in self.slots.keys() {
            if !vivos.contains(id) && !self.orphans.contains(id) {
                self.orphans.push(*id);
            }
        }
        while self.orphans.len() > self.cap {
            let viejo = self.orphans.remove(0);
            self.slots.remove(&viejo);
        }
    }

    /// Los estados, en orden de [`SlotId`].
    pub fn values(&self) -> impl Iterator<Item = &P> {
        self.slots.values()
    }

    /// Los estados, en orden de [`SlotId`], para mutarlos.
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut P> {
        self.slots.values_mut()
    }

    /// Los pares `(id, estado)`, en orden de [`SlotId`].
    pub fn iter(&self) -> impl Iterator<Item = (SlotId, &P)> {
        self.slots.iter().map(|(id, p)| (*id, p))
    }

    /// Los pares `(id, estado)`, en orden de [`SlotId`], para mutarlos.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (SlotId, &mut P)> {
        self.slots.iter_mut().map(|(id, p)| (*id, p))
    }

    /// Intercambia el estado de dos huecos, dejando los ids donde estaban.
    ///
    /// Lo pide el gesto de intercambiar paneles: lo que cambia de sitio es el
    /// CONTENIDO, no la identidad del hueco — si se movieran los ids, todo lo
    /// que guarda un `SlotId` de antes pasaría a nombrar al otro.
    pub fn swap(&mut self, a: SlotId, b: SlotId) {
        if a == b {
            return;
        }
        let (va, vb) = (self.slots.remove(&a), self.slots.remove(&b));
        if let Some(v) = vb {
            self.slots.insert(a, v);
        }
        if let Some(v) = va {
            self.slots.insert(b, v);
        }
    }

    /// Los ids huérfanos, del más antiguo al más reciente.
    #[must_use]
    pub fn orphans(&self) -> Vec<SlotId> {
        self.orphans.clone()
    }

    /// Cuántos estados hay guardados, vivos y huérfanos.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// ¿Ninguno?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Dir, KindId, Size};

    fn browser(id: u32) -> Node {
        Node::slot(SlotId(id), KindId::browser())
    }
    fn dos(a: u32, b: u32) -> Node {
        Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![browser(a), browser(b)],
        }
    }

    /// Cerrar un hueco NO borra su estado: queda huérfano, para que reabrir la
    /// misma disposición recupere el historial en vez de arrancar en blanco.
    #[test]
    fn cerrar_un_hueco_deja_su_estado_huerfano() {
        let mut s: SlotStore<u32> = SlotStore::default();
        s.insert(SlotId(1), 10);
        s.insert(SlotId(2), 20);
        s.sync_with(&browser(1));
        assert_eq!(s.get(SlotId(2)), Some(&20), "el estado del cerrado sigue");
        assert_eq!(s.orphans(), vec![SlotId(2)]);
    }

    /// Los huérfanos tienen tope: sin él, abrir y cerrar paneles toda una
    /// sesión crece sin fin. Se purga el MÁS ANTIGUO.
    #[test]
    fn los_huerfanos_tienen_tope_y_se_purga_el_mas_antiguo() {
        let mut s: SlotStore<u32> = SlotStore::with_orphan_cap(2);
        for i in 1..=4 {
            s.insert(SlotId(i), i);
        }
        s.sync_with(&browser(4));
        assert_eq!(s.orphans().len(), 2);
        assert!(s.get(SlotId(1)).is_none(), "el más antiguo se fue");
        assert_eq!(s.get(SlotId(4)), Some(&4), "el vivo no se toca");
    }

    /// Reabrir un id huérfano lo revive con su estado.
    #[test]
    fn reabrir_un_id_huerfano_recupera_su_estado() {
        let mut s: SlotStore<u32> = SlotStore::default();
        s.insert(SlotId(1), 10);
        s.insert(SlotId(2), 20);
        s.sync_with(&browser(1));
        s.sync_with(&dos(1, 2));
        assert_eq!(s.get(SlotId(2)), Some(&20));
        assert!(s.orphans().is_empty());
    }

    /// Intercambiar mueve el CONTENIDO y deja los ids quietos: si se movieran
    /// los ids, cualquier `SlotId` guardado de antes nombraría al otro hueco.
    #[test]
    fn intercambiar_mueve_el_contenido_no_los_ids() {
        let mut s: SlotStore<u32> = SlotStore::default();
        s.insert(SlotId(1), 10);
        s.insert(SlotId(2), 20);
        s.swap(SlotId(1), SlotId(2));
        assert_eq!(s.get(SlotId(1)), Some(&20));
        assert_eq!(s.get(SlotId(2)), Some(&10));
        assert_eq!(s.values().copied().collect::<Vec<_>>(), vec![20, 10]);
    }

    /// Dos `sync_with` seguidos sin cambios no duplican el huérfano: si lo
    /// hicieran, el tope se agotaría con un solo hueco cerrado y un frame
    /// repetido bastaría para tirar estado vivo.
    #[test]
    fn sincronizar_dos_veces_no_duplica_al_huerfano() {
        let mut s: SlotStore<u32> = SlotStore::default();
        s.insert(SlotId(1), 10);
        s.insert(SlotId(2), 20);
        s.sync_with(&browser(1));
        s.sync_with(&browser(1));
        assert_eq!(s.orphans(), vec![SlotId(2)]);
    }
}
