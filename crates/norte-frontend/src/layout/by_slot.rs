//! Estado indexado por hueco, con la ergonomía del array que sustituye.

use std::collections::BTreeMap;

use super::{Node, SlotId};

/// Estado por hueco.
///
/// Existe porque el bucle de un frontend guarda cosas POR PANE —el relleno
/// paginado en vuelo, la decoración pedida, la deduplicación de la sonda de
/// stat, la búsqueda viva— y guardarlas por POSICIÓN es dos problemas: un tope
/// de dos paneles, y un fallo silencioso. Una respuesta en vuelo para la
/// posición 1 se aplica a quien esté en la posición 1 cuando llegue, que tras
/// cerrar un panel es otro. Por eso la clave es el hueco, que no se mueve.
#[derive(Debug, Clone)]
pub struct BySlot<T> {
    inner: BTreeMap<SlotId, T>,
}

impl<T> Default for BySlot<T> {
    fn default() -> Self {
        Self {
            inner: BTreeMap::new(),
        }
    }
}

impl<T> BySlot<T> {
    /// Vacío.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// El valor del hueco, si lo hay.
    #[must_use]
    pub fn get(&self, id: SlotId) -> Option<&T> {
        self.inner.get(&id)
    }

    /// El valor del hueco, para mutarlo.
    pub fn get_mut(&mut self, id: SlotId) -> Option<&mut T> {
        self.inner.get_mut(&id)
    }

    /// Pone el valor de un hueco y devuelve el anterior.
    pub fn insert(&mut self, id: SlotId, v: T) -> Option<T> {
        self.inner.insert(id, v)
    }

    /// Quita el valor de un hueco.
    pub fn remove(&mut self, id: SlotId) -> Option<T> {
        self.inner.remove(&id)
    }

    /// ¿Hay algo para ese hueco?
    #[must_use]
    pub fn contains(&self, id: SlotId) -> bool {
        self.inner.contains_key(&id)
    }

    /// Cuántos huecos tienen valor.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// ¿Ninguno?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Los pares, en orden de [`SlotId`].
    pub fn iter(&self) -> impl Iterator<Item = (SlotId, &T)> {
        self.inner.iter().map(|(id, v)| (*id, v))
    }

    /// Los pares, para mutarlos.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (SlotId, &mut T)> {
        self.inner.iter_mut().map(|(id, v)| (*id, v))
    }

    /// Tira lo que `tree` ya no menciona.
    ///
    /// Se llama tras cada cambio de layout. A diferencia de
    /// [`super::SlotStore`], aquí NO se conserva lo huérfano: el estado del
    /// store es del usuario —su cursor, sus marcas— y merece sobrevivir a un
    /// cierre por error; esto de aquí es trabajo EN VUELO, y aplicar el
    /// resultado de una petición a un panel que ya no existe no es recuperar
    /// nada, es actuar sobre un fantasma.
    pub fn retain_tree(&mut self, tree: &Node) {
        let vivos = tree.slot_ids();
        self.inner.retain(|id, _| vivos.contains(id));
    }
}

impl<T: Default> BySlot<T> {
    /// El valor del hueco, creándolo por defecto si no estaba.
    pub fn entry(&mut self, id: SlotId) -> &mut T {
        self.inner.entry(id).or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Dir, KindId};

    fn b(id: u32) -> Node {
        Node::slot(SlotId(id), KindId::browser())
    }

    /// Lo que el árbol ya no menciona se TIRA. Aplicar el resultado de una
    /// petición a un panel que se cerró no es recuperar nada: es actuar sobre
    /// un fantasma, y en la posición de al lado.
    #[test]
    fn lo_que_el_arbol_no_menciona_se_tira() {
        let mut m: BySlot<u32> = BySlot::new();
        m.insert(SlotId(1), 10);
        m.insert(SlotId(2), 20);
        m.retain_tree(&b(1));
        assert_eq!(m.get(SlotId(1)), Some(&10));
        assert_eq!(m.get(SlotId(2)), None);
    }

    /// Una pestaña OCULTA sigue en el árbol, así que su trabajo en vuelo no se
    /// tira: sigue siendo suyo y sigue teniendo dónde aterrizar.
    #[test]
    fn una_pestana_oculta_conserva_lo_suyo() {
        let mut m: BySlot<u32> = BySlot::new();
        m.insert(SlotId(2), 20);
        let arbol = Node::Tabs {
            children: vec![b(1), b(2)],
            active: 0,
        };
        m.retain_tree(&arbol);
        assert_eq!(m.get(SlotId(2)), Some(&20));
    }

    #[test]
    fn entry_crea_por_defecto_y_iter_va_en_orden() {
        let mut m: BySlot<u32> = BySlot::new();
        *m.entry(SlotId(3)) += 1;
        *m.entry(SlotId(1)) += 5;
        assert_eq!(
            m.iter().map(|(id, v)| (id.0, *v)).collect::<Vec<_>>(),
            vec![(1, 5), (3, 1)]
        );
    }

    /// Un `Split` no cambia nada: lo que decide es qué huecos hay, no cómo se
    /// reparten.
    #[test]
    fn la_forma_del_arbol_no_decide_nada_aqui() {
        let mut m: BySlot<u32> = BySlot::new();
        m.insert(SlotId(1), 1);
        m.insert(SlotId(2), 2);
        m.retain_tree(&Node::split(Dir::Horizontal, vec![b(1), b(2)]));
        assert_eq!(m.len(), 2);
    }
}
