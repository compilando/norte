//! El sistema de ventanas: un árbol de huecos que los frontends reparten desde
//! el mismo código puro.
//!
//! La pantalla de norte estaba escrita a mano —`draw` partía el frame y
//! `pane_geometry`/`pane_list_rows` REPLICABAN esa aritmética para el ratón y
//! la paginación—, y era correcta precisamente porque era fija: no tenía sitio
//! para una quinta cosa. Aquí vive lo que la sustituye.
//!
//! # Cuatro conceptos que no se solapan
//!
//! - **Hueco** ([`Node::Slot`]) — dónde va un panel, con su identidad.
//! - **Kind** ([`KindId`]) — QUÉ panel es. Un string, no un enum, para que un
//!   plugin pueda aportar uno.
//! - **Rol** ([`RoleId`]) — QUIÉN es quién: `active` y `target`, punteros
//!   resueltos en cada frame.
//! - **Vínculo** ([`Bindings`]) — DE QUIÉN es vista un hueco. Sin esto, un
//!   panel auxiliar es una caja sin nada dentro.
//!
//! Las pestañas ([`Node::Tabs`]) no son un concepto aparte: son un tipo de
//! nodo, y dónde caen en el árbol decide si son espacios de trabajo, pestañas
//! de panel, o media pantalla alternando vistas.
//!
//! La decisión y sus alternativas descartadas están en la ADR 0058; el diseño,
//! en `docs/superpowers/specs/2026-08-17-layout-slots-tabs-design.md`.

mod kinds;
mod tree;

pub use kinds::{KindDecl, KindRegistry};
pub use tree::{Bindings, Dir, Follow, KindId, Node, Params, Rect, RoleId, SlotId};

/// Lo que impide usar un layout.
///
/// Se distingue del diagnóstico igual que el keymap distingue
/// [`crate::keymap::KeymapError`] de [`crate::keymap::KeymapDiagnostic`]: un
/// error deja el layout anterior en pie (o cae al preset por defecto en
/// arranque frío), un diagnóstico se arregla solo y se CUENTA.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LayoutError {
    /// Dos huecos con el mismo id. No se adivina cuál gana.
    #[error("dos huecos con el mismo id: {0:?}")]
    DuplicateSlotId(SlotId),
    /// Un `Split` sin hijos no reparte nada.
    #[error("un `Split` sin hijos")]
    EmptySplit,
    /// Los pesos son índice-paralelos a los hijos.
    #[error("{count} pesos para {children} hijos")]
    WeightsMismatch {
        /// Cuántos pesos había.
        count: usize,
        /// Cuántos hijos hay.
        children: usize,
    },
}

/// Lo que se arregla solo y hay que CONTAR. `norte doctor` los muestra.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutDiagnostic {
    /// La pestaña activa estaba fuera de rango.
    ActiveClamped {
        /// El índice que traía.
        was: usize,
        /// A cuál se clampó.
        to: usize,
    },
    /// Un peso de cero no reparte nada; se sube a uno.
    ZeroWeightRaised {
        /// Posición del hijo dentro de su `Split`.
        at: usize,
    },
    /// Un `follows` apuntaba a un hueco que no existe; pasa a seguir al rol
    /// `active`, que es lo que se quería el 95 % de las veces.
    FollowRetargeted {
        /// El hueco cuyo vínculo se redirigió.
        slot: SlotId,
    },
}

/// Valida un árbol antes de usarlo.
///
/// Solo lo INCOHERENTE: lo clampable (una pestaña activa fuera de rango, un
/// peso de cero) no es un error, se arregla durante el reparto y se cuenta
/// como [`LayoutDiagnostic`].
///
/// # Errors
///
/// [`LayoutError`] si hay ids repetidos, un `Split` sin hijos, o pesos que no
/// son índice-paralelos a los hijos.
pub fn validate(tree: &Node) -> Result<(), LayoutError> {
    if let Some(id) = tree.duplicate_slot_ids().first() {
        return Err(LayoutError::DuplicateSlotId(*id));
    }
    validate_shape(tree)
}

fn validate_shape(node: &Node) -> Result<(), LayoutError> {
    match node {
        Node::Split {
            children, weights, ..
        } => {
            if children.is_empty() {
                return Err(LayoutError::EmptySplit);
            }
            if weights.len() != children.len() {
                return Err(LayoutError::WeightsMismatch {
                    count: weights.len(),
                    children: children.len(),
                });
            }
            for c in children {
                validate_shape(c)?;
            }
            Ok(())
        }
        Node::Tabs { children, .. } => {
            if children.is_empty() {
                return Err(LayoutError::EmptySplit);
            }
            for c in children {
                validate_shape(c)?;
            }
            Ok(())
        }
        Node::Slot { .. } => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_arbol_con_ids_repetidos_no_se_usa() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            weights: vec![1, 1],
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(1), KindId::browser()),
            ],
        };
        assert_eq!(
            validate(&arbol),
            Err(LayoutError::DuplicateSlotId(SlotId(1)))
        );
    }

    /// Los pesos son índice-paralelos: uno de menos y el reparto pintaría un
    /// hueco donde no toca en vez de fallar.
    #[test]
    fn los_pesos_tienen_que_ser_tantos_como_hijos() {
        let arbol = Node::Split {
            dir: Dir::Horizontal,
            weights: vec![1],
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        };
        assert_eq!(
            validate(&arbol),
            Err(LayoutError::WeightsMismatch {
                count: 1,
                children: 2
            })
        );
    }

    #[test]
    fn un_arbol_sano_valida() {
        let arbol = Node::Split {
            dir: Dir::Horizontal,
            weights: vec![1, 1],
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        };
        assert_eq!(validate(&arbol), Ok(()));
    }
}
