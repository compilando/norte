//! Los roles y los vínculos: quién es quién, y de quién es vista cada hueco.

use std::collections::BTreeMap;

use super::{Follow, KindRegistry, LayoutDiagnostic, Node, Resolved, RoleId, SlotId};

/// Los punteros con nombre dentro del árbol.
///
/// `active` es el foco; `target` es a dónde va una operación que necesita un
/// segundo sitio. Se resuelven en cada frame porque son afirmaciones sobre lo
/// que hay AHORA en pantalla, no propiedades guardadas del layout.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roles(BTreeMap<RoleId, SlotId>);

impl Roles {
    /// Quién tiene el rol `role`, si alguien.
    #[must_use]
    pub fn get(&self, role: RoleId) -> Option<SlotId> {
        self.0.get(&role).copied()
    }

    /// Da el rol `role` a `slot`.
    pub fn set(&mut self, role: RoleId, slot: SlotId) {
        self.0.insert(role, slot);
    }

    /// Quita el rol `role` a quien lo tuviera.
    pub fn clear(&mut self, role: RoleId) {
        self.0.remove(&role);
    }

    /// Solo el foco. Atajo para el arranque y para los tests.
    #[must_use]
    pub fn con_active(slot: SlotId) -> Self {
        let mut r = Self::default();
        r.set(RoleId::Active, slot);
        r
    }

    /// Deja los roles coherentes con lo que hay en pantalla. Se llama tras
    /// CADA `resolve`.
    ///
    /// - `active` pasa a ser `foco`, siempre.
    /// - `target` se conserva si sigue visible y elegible. Si no, se reubica al
    ///   ÚNICO otro candidato visible; con cero o con varios se queda SIN
    ///   fijar, y quien lo necesite pedirá una ruta.
    ///
    /// Ese último punto es la regla, no un detalle: con dos panes el destino
    /// es obvio y nadie nota que el concepto existe, pero con varios —o con
    /// uno detrás de una pestaña— una copia hacia el que el motor desempate
    /// solo es pérdida de datos silenciosa (ADR 0058 D7).
    pub fn reconcile(
        &mut self,
        tree: &Node,
        resolved: &Resolved,
        decls: &KindRegistry,
        foco: SlotId,
    ) {
        self.set(RoleId::Active, foco);
        let candidatos: Vec<SlotId> = resolved
            .placements
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| *id != foco)
            .filter(|id| {
                tree.kind_of(*id)
                    .is_some_and(|k| decls.holds_role(k, RoleId::Target))
            })
            .collect();
        match self.get(RoleId::Target) {
            Some(actual) if candidatos.contains(&actual) => {}
            _ => {
                if let [unico] = candidatos[..] {
                    self.set(RoleId::Target, unico);
                } else {
                    self.clear(RoleId::Target);
                }
            }
        }
    }
}

/// A qué hueco mira el hueco `de`. `None` = no mira a nadie.
///
/// Un `follows` a un hueco que ya no existe degrada a seguir al rol `active` y
/// deja un [`LayoutDiagnostic::FollowRetargeted`]: es lo que se quería casi
/// siempre, y dejarlo roto en silencio es un panel auxiliar mirando al vacío
/// sin que nada lo diga.
#[must_use]
pub fn resolve_follow(
    tree: &Node,
    de: SlotId,
    roles: &Roles,
    diags: &mut Vec<LayoutDiagnostic>,
) -> Option<SlotId> {
    match tree.bindings_of(de).and_then(|b| b.follows) {
        None => None,
        Some(Follow::Role(r)) => roles.get(r),
        Some(Follow::Slot(s)) => {
            if tree.slot_ids().contains(&s) {
                Some(s)
            } else {
                diags.push(LayoutDiagnostic::FollowRetargeted { slot: de });
                roles.get(RoleId::Active)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Bindings, Dir, KindId, Params, Rect, resolve};

    fn reg() -> KindRegistry {
        KindRegistry::builtin()
    }
    fn browser(id: u32) -> Node {
        Node::slot(SlotId(id), KindId::browser())
    }
    fn pintado(arbol: Node) -> (Node, Resolved) {
        let res = resolve(Rect::new(0, 0, 100, 30), &arbol, &reg());
        (arbol, res)
    }
    fn split(children: Vec<Node>) -> Node {
        let weights = vec![1; children.len()];
        Node::Split {
            dir: Dir::Horizontal,
            children,
            weights,
        }
    }

    /// Con dos browsers, `target` es el otro. Eso es lo que hace que el layout
    /// ortodoxo se comporte EXACTAMENTE como hoy con el concepto ya presente
    /// pero invisible: `F5` copia al otro pane y nadie se entera.
    #[test]
    fn con_dos_browsers_el_destino_es_el_otro() {
        let (arbol, res) = pintado(split(vec![browser(1), browser(2)]));
        let mut roles = Roles::default();
        roles.reconcile(&arbol, &res, &reg(), SlotId(1));
        assert_eq!(roles.get(RoleId::Active), Some(SlotId(1)));
        assert_eq!(roles.get(RoleId::Target), Some(SlotId(2)));
    }

    /// Con UN solo browser no hay destino, y eso NO es un estado roto: la
    /// operación que lo necesite pedirá una ruta.
    #[test]
    fn con_un_solo_browser_no_hay_destino() {
        let (arbol, res) = pintado(browser(1));
        let mut roles = Roles::default();
        roles.reconcile(&arbol, &res, &reg(), SlotId(1));
        assert_eq!(roles.get(RoleId::Target), None);
    }

    /// El destino se OCULTA (cambio de pestaña): el rol se reubica al
    /// candidato visible. Un destino detrás de una pestaña es pérdida de datos
    /// silenciosa.
    #[test]
    fn un_destino_que_se_oculta_se_reubica() {
        let (arbol, res) = pintado(split(vec![
            browser(1),
            Node::Tabs {
                children: vec![browser(2), browser(3)],
                active: 1,
            },
        ]));
        assert_eq!(res.hidden, vec![SlotId(2)], "el 2 está oculto");
        let mut roles = Roles::default();
        roles.set(RoleId::Target, SlotId(2));
        roles.reconcile(&arbol, &res, &reg(), SlotId(1));
        assert_eq!(
            roles.get(RoleId::Target),
            Some(SlotId(3)),
            "se reubica al visible"
        );
    }

    /// Con TRES browsers visibles y ninguno designado, no hay default: dos
    /// candidatos no se desempatan solos.
    #[test]
    fn con_varios_candidatos_no_hay_destino_por_defecto() {
        let (arbol, res) = pintado(split(vec![browser(1), browser(2), browser(3)]));
        let mut roles = Roles::default();
        roles.reconcile(&arbol, &res, &reg(), SlotId(1));
        assert_eq!(roles.get(RoleId::Target), None);
    }

    /// Pero un destino designado A MANO se respeta aunque haya varios: el
    /// motor no desempata, el usuario sí.
    #[test]
    fn un_destino_designado_a_mano_sobrevive_a_la_reconciliacion() {
        let (arbol, res) = pintado(split(vec![browser(1), browser(2), browser(3)]));
        let mut roles = Roles::default();
        roles.set(RoleId::Target, SlotId(3));
        roles.reconcile(&arbol, &res, &reg(), SlotId(1));
        assert_eq!(roles.get(RoleId::Target), Some(SlotId(3)));
    }

    /// Un kind que no puede tomar el rol no es candidato aunque esté visible:
    /// `tasks` jamás es destino de una copia.
    #[test]
    fn un_kind_sin_ese_rol_no_es_candidato() {
        let (arbol, res) = pintado(split(vec![
            browser(1),
            Node::slot(SlotId(2), KindId::new("tasks")),
        ]));
        let mut roles = Roles::default();
        roles.reconcile(&arbol, &res, &reg(), SlotId(1));
        assert_eq!(roles.get(RoleId::Target), None);
    }

    /// Un `follows` roto degrada a seguir al rol `active` y lo CUENTA.
    #[test]
    fn un_follow_a_un_hueco_que_no_existe_degrada_a_active() {
        let arbol = Node::Slot {
            id: SlotId(1),
            kind: KindId::new("metadata"),
            params: Params::new(),
            bindings: Bindings {
                follows: Some(Follow::Slot(SlotId(99))),
            },
        };
        let mut diags = vec![];
        let objetivo = resolve_follow(&arbol, SlotId(1), &Roles::con_active(SlotId(1)), &mut diags);
        assert_eq!(objetivo, Some(SlotId(1)));
        assert!(
            diags
                .iter()
                .any(|d| matches!(d, LayoutDiagnostic::FollowRetargeted { .. }))
        );
    }

    /// Un `follows: Role(Active)` sigue al foco, que es el default útil de un
    /// panel de metadatos o de un preview acoplado.
    #[test]
    fn un_follow_al_rol_active_sigue_al_foco() {
        let arbol = Node::Slot {
            id: SlotId(1),
            kind: KindId::new("metadata"),
            params: Params::new(),
            bindings: Bindings {
                follows: Some(Follow::Role(RoleId::Active)),
            },
        };
        let mut diags = vec![];
        let objetivo = resolve_follow(&arbol, SlotId(1), &Roles::con_active(SlotId(5)), &mut diags);
        assert_eq!(objetivo, Some(SlotId(5)));
        assert!(diags.is_empty());
    }
}
