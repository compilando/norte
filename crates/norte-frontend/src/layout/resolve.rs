//! El reparto: árbol + área → quién se pinta, dónde, y quién no.

use super::{Dir, KindRegistry, LayoutDiagnostic, Node, Rect, SlotId};

/// Lo que un frame necesita saber, en un solo valor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolved {
    /// Quién se pinta y dónde, en orden de pintado.
    pub placements: Vec<(SlotId, Rect)>,
    /// Quién NO se pinta: pestaña inactiva, o colapsado fuera.
    pub hidden: Vec<SlotId>,
    /// En qué orden se tabula. Ya sin subárboles ocultos.
    pub focus_order: Vec<SlotId>,
    /// Lo que hubo que arreglar sobre la marcha.
    pub diagnostics: Vec<LayoutDiagnostic>,
}

/// Reparte `area` entre los huecos de `tree`.
///
/// El colapso vive aquí y MUERE con el frame: un `Split` cuyos hijos no llegan
/// a su mínimo declarado se degrada a pestañas para este frame y propaga hacia
/// arriba si sigue sin caber. El árbol de entrada no se toca jamás — un layout
/// guardado es la intención del usuario, y reescribirlo por el tamaño de la
/// pantalla significa que abrir el TUI un minuto machaca el layout de la GUI
/// (ADR 0058 D5).
#[must_use]
pub fn resolve(area: Rect, tree: &Node, decls: &KindRegistry) -> Resolved {
    let mut out = Resolved::default();
    place(tree, area, decls, &mut out);
    out
}

/// Lo que exige un subárbol ENTERO para caber.
///
/// Un `Split` suma a lo largo de su eje y toma el máximo en el otro; un `Tabs`
/// toma el máximo en los dos, porque sus hijos comparten el mismo sitio.
fn min_of(node: &Node, decls: &KindRegistry) -> (u16, u16) {
    match node {
        Node::Slot { kind, .. } => decls.min_of(kind),
        Node::Tabs { children, .. } => children
            .iter()
            .map(|c| min_of(c, decls))
            .fold((0, 0), |(w, h), (cw, ch)| (w.max(cw), h.max(ch))),
        Node::Split { dir, children, .. } => {
            children
                .iter()
                .map(|c| min_of(c, decls))
                .fold((0, 0), |(w, h), (cw, ch)| match dir {
                    Dir::Horizontal => (w.saturating_add(cw), h.max(ch)),
                    Dir::Vertical => (w.max(cw), h.saturating_add(ch)),
                })
        }
    }
}

/// Reparte `area` en trozos proporcionales a `pesos` a lo largo de `dir`.
///
/// El resto va al ÚLTIMO: con un ancho impar la división entera perdería una
/// columna, y una columna sin pintar en el borde de un pane se ve.
fn distribute(area: Rect, dir: Dir, pesos: &[u32]) -> Vec<Rect> {
    let total: u32 = pesos.iter().sum::<u32>().max(1);
    let extent = u32::from(match dir {
        Dir::Horizontal => area.width,
        Dir::Vertical => area.height,
    });
    let mut out = Vec::with_capacity(pesos.len());
    let mut usado: u32 = 0;
    for (i, p) in pesos.iter().enumerate() {
        let size = if i + 1 == pesos.len() {
            extent.saturating_sub(usado)
        } else {
            extent.saturating_mul(*p) / total
        };
        let off = u16::try_from(usado).unwrap_or(u16::MAX);
        let size16 = u16::try_from(size).unwrap_or(u16::MAX);
        out.push(match dir {
            Dir::Horizontal => Rect::new(area.x.saturating_add(off), area.y, size16, area.height),
            Dir::Vertical => Rect::new(area.x, area.y.saturating_add(off), area.width, size16),
        });
        usado = usado.saturating_add(size);
    }
    out
}

/// Coloca `node` en `area`, acumulando en `out`.
fn place(node: &Node, area: Rect, decls: &KindRegistry, out: &mut Resolved) {
    match node {
        Node::Slot { id, kind, .. } => {
            out.placements.push((*id, area));
            if decls.get(kind).is_some_and(|d| d.focusable) {
                out.focus_order.push(*id);
            }
        }
        Node::Tabs { children, active } => {
            let idx = if *active < children.len() {
                *active
            } else {
                out.diagnostics.push(LayoutDiagnostic::ActiveClamped {
                    was: *active,
                    to: 0,
                });
                0
            };
            solo_uno(children, idx, area, decls, out);
        }
        Node::Split {
            dir,
            children,
            weights,
        } => {
            let pesos: Vec<u32> = (0..children.len())
                .map(|i| {
                    let w = weights.get(i).copied().unwrap_or(1);
                    if w == 0 {
                        out.diagnostics
                            .push(LayoutDiagnostic::ZeroWeightRaised { at: i });
                    }
                    u32::from(w.max(1))
                })
                .collect();
            let rects = distribute(area, *dir, &pesos);
            let cabe = children.iter().zip(&rects).all(|(c, r)| {
                let (mw, mh) = min_of(c, decls);
                r.width >= mw && r.height >= mh
            });
            if cabe {
                for (c, r) in children.iter().zip(rects) {
                    place(c, r, decls, out);
                }
            } else {
                // Degradado a pestañas PARA ESTE FRAME. Si el hijo que queda
                // tampoco cabe, colapsará él a su vez: así propaga hacia
                // arriba sin que nadie tenga que contar niveles.
                solo_uno(children, 0, area, decls, out);
            }
        }
    }
}

/// Coloca el hijo `idx` en todo el área y manda los huecos del resto a
/// `hidden` — que es la señal de suspensión, no un detalle de pintado.
fn solo_uno(children: &[Node], idx: usize, area: Rect, decls: &KindRegistry, out: &mut Resolved) {
    for (i, c) in children.iter().enumerate() {
        if i == idx {
            place(c, area, decls, out);
        } else {
            out.hidden.extend(c.slot_ids());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{KindId, LayoutDiagnostic};
    use proptest::prelude::*;

    fn reg() -> KindRegistry {
        KindRegistry::builtin()
    }
    fn r(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect::new(x, y, w, h)
    }
    fn dos(a: Node, b: Node, dir: Dir) -> Node {
        Node::Split {
            dir,
            weights: vec![1, 1],
            children: vec![a, b],
        }
    }
    fn browser(id: u32) -> Node {
        Node::slot(SlotId(id), KindId::browser())
    }

    /// El caso `orthodox`: dos browsers al 50 %. Es la pantalla de hoy
    /// expresada en el modelo nuevo, y que sea expresable es la prueba de que
    /// el modelo está bien puesto.
    #[test]
    fn dos_browsers_al_cincuenta_por_ciento() {
        let arbol = dos(browser(1), browser(2), Dir::Horizontal);
        let out = resolve(r(0, 0, 100, 30), &arbol, &reg());
        assert_eq!(
            out.placements,
            vec![(SlotId(1), r(0, 0, 50, 30)), (SlotId(2), r(50, 0, 50, 30))]
        );
        assert!(out.hidden.is_empty());
        assert_eq!(out.focus_order, vec![SlotId(1), SlotId(2)]);
    }

    /// Un ancho impar no puede perder una columna: el resto va al último.
    #[test]
    fn un_ancho_impar_no_pierde_una_columna() {
        let arbol = dos(browser(1), browser(2), Dir::Horizontal);
        let out = resolve(r(0, 0, 101, 30), &arbol, &reg());
        let ancho: u16 = out.placements.iter().map(|(_, re)| re.width).sum();
        assert_eq!(ancho, 101, "se perdió una columna en el reparto");
    }

    /// Un corte VERTICAL reparte el alto, y las `y` van encadenadas.
    #[test]
    fn un_corte_vertical_reparte_el_alto() {
        let arbol = dos(browser(1), browser(2), Dir::Vertical);
        let out = resolve(r(0, 0, 40, 20), &arbol, &reg());
        assert_eq!(
            out.placements,
            vec![(SlotId(1), r(0, 0, 40, 10)), (SlotId(2), r(0, 10, 40, 10))]
        );
    }

    /// Solo la pestaña ACTIVA se coloca; las otras van a `hidden`, que es la
    /// señal de suspensión (fuera watches, sondas y columnas de plugin).
    #[test]
    fn una_pestana_inactiva_va_a_hidden_no_a_placements() {
        let arbol = Node::Tabs {
            active: 1,
            children: vec![browser(1), browser(2)],
        };
        let out = resolve(r(0, 0, 100, 30), &arbol, &reg());
        assert_eq!(out.placements, vec![(SlotId(2), r(0, 0, 100, 30))]);
        assert_eq!(out.hidden, vec![SlotId(1)]);
        assert_eq!(
            out.focus_order,
            vec![SlotId(2)],
            "no se tabula a lo que no se ve"
        );
    }

    /// El colapso: dos browsers de mínimo 20 no caben en 30 columnas, así que
    /// el `Split` se degrada a `Tabs` PARA ESTE FRAME. El árbol no se toca.
    #[test]
    fn un_split_que_no_cabe_colapsa_a_pestanas_sin_tocar_el_arbol() {
        let arbol = dos(browser(1), browser(2), Dir::Horizontal);
        let antes = arbol.clone();
        let out = resolve(r(0, 0, 30, 30), &arbol, &reg());
        assert_eq!(out.placements.len(), 1, "solo uno cabe");
        assert_eq!(out.hidden, vec![SlotId(2)]);
        assert_eq!(arbol, antes, "resolve NO puede mutar el árbol");
    }

    /// El colapso PROPAGA: si tras degradar un nivel sigue sin caber, degrada
    /// el de arriba. Sin esto, una ventana muy estrecha pinta cajas de dos
    /// columnas en vez de una pantalla usable.
    #[test]
    fn el_colapso_propaga_hacia_arriba() {
        let arbol = dos(
            dos(browser(1), browser(2), Dir::Horizontal),
            browser(3),
            Dir::Horizontal,
        );
        let out = resolve(r(0, 0, 30, 30), &arbol, &reg());
        assert_eq!(out.placements.len(), 1);
        assert_eq!(out.placements[0].1, r(0, 0, 30, 30), "ocupa todo");
    }

    /// No cabe NADA: se pinta igual, incumpliendo el mínimo. Nunca pantalla en
    /// blanco — un usuario con un terminal diminuto ve algo y un mensaje, no
    /// un vacío que parece un cuelgue.
    #[test]
    fn cuando_no_cabe_nada_se_pinta_uno_igualmente() {
        let arbol = browser(1);
        let out = resolve(r(0, 0, 6, 2), &arbol, &reg());
        assert_eq!(out.placements, vec![(SlotId(1), r(0, 0, 6, 2))]);
        assert!(out.hidden.is_empty());
    }

    /// Un kind fuera del registro se coloca igual (caja con su nombre) pero no
    /// entra en el orden de foco: no se tabula a algo que nadie sabe pintar.
    #[test]
    fn un_kind_desconocido_se_coloca_pero_no_toma_foco() {
        let arbol = Node::slot(SlotId(9), KindId::new("terminal"));
        let out = resolve(r(0, 0, 100, 30), &arbol, &reg());
        assert_eq!(out.placements.len(), 1);
        assert!(out.focus_order.is_empty());
    }

    /// `tasks` se coloca pero no toma foco: se mira, no se enfoca.
    #[test]
    fn tasks_se_pinta_pero_no_se_tabula() {
        let arbol = dos(
            browser(1),
            Node::slot(SlotId(2), KindId::new("tasks")),
            Dir::Vertical,
        );
        let out = resolve(r(0, 0, 100, 30), &arbol, &reg());
        assert_eq!(out.placements.len(), 2);
        assert_eq!(out.focus_order, vec![SlotId(1)]);
    }

    /// `active` fuera de rango se clampa y se CUENTA; no es un error duro.
    #[test]
    fn un_active_fuera_de_rango_se_clampa_con_diagnostico() {
        let arbol = Node::Tabs {
            active: 7,
            children: vec![browser(1)],
        };
        let out = resolve(r(0, 0, 100, 30), &arbol, &reg());
        assert_eq!(out.placements.len(), 1);
        assert!(
            out.diagnostics
                .iter()
                .any(|d| matches!(d, LayoutDiagnostic::ActiveClamped { .. }))
        );
    }

    /// Un peso de cero no reparte nada: se sube a uno y se cuenta.
    #[test]
    fn un_peso_de_cero_se_sube_a_uno_con_diagnostico() {
        let arbol = Node::Split {
            dir: Dir::Horizontal,
            weights: vec![0, 1],
            children: vec![browser(1), browser(2)],
        };
        let out = resolve(r(0, 0, 100, 30), &arbol, &reg());
        assert_eq!(out.placements.len(), 2, "el de peso cero también se pinta");
        assert!(
            out.diagnostics
                .iter()
                .any(|d| matches!(d, LayoutDiagnostic::ZeroWeightRaised { .. }))
        );
    }

    // --- propiedades ---

    fn se_solapan(a: Rect, b: Rect) -> bool {
        a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
    }

    fn arbol_arbitrario() -> impl Strategy<Value = Node> {
        let kinds = prop::sample::select(vec![
            "browser", "tasks", "viewer", "compare", "sync", "terminal",
        ]);
        let hoja = (0u32..64, kinds).prop_map(|(id, k)| Node::slot(SlotId(id), KindId::new(k)));
        hoja.prop_recursive(3, 24, 4, |inner| {
            prop_oneof![
                (
                    prop::collection::vec(inner.clone(), 1..4),
                    prop::collection::vec(0u16..5, 1..4),
                    any::<bool>()
                )
                    .prop_map(|(children, mut weights, horiz)| {
                        weights.resize(children.len(), 1);
                        Node::Split {
                            dir: if horiz {
                                Dir::Horizontal
                            } else {
                                Dir::Vertical
                            },
                            children,
                            weights,
                        }
                    }),
                (prop::collection::vec(inner, 1..4), 0usize..5)
                    .prop_map(|(children, active)| Node::Tabs { children, active }),
            ]
        })
    }

    proptest! {
        /// Las colocaciones NUNCA se solapan y nunca se salen del área. En un
        /// TUI un solape no se ve como un bug de layout: se ve como texto
        /// corrupto, y se persigue en el sitio equivocado.
        #[test]
        fn las_colocaciones_ni_se_solapan_ni_se_salen(
            arbol in arbol_arbitrario(), w in 1u16..200, h in 1u16..80
        ) {
            let out = resolve(Rect::new(0, 0, w, h), &arbol, &reg());
            for (i, (_, a)) in out.placements.iter().enumerate() {
                prop_assert!(a.x + a.width <= w && a.y + a.height <= h);
                for (_, b) in out.placements.iter().skip(i + 1) {
                    prop_assert!(!se_solapan(*a, *b), "{a:?} solapa {b:?}");
                }
            }
        }

        /// Todo hueco colocado cumple su mínimo, salvo en el caso «no cabe
        /// nada», que se reconoce porque solo hay UNA colocación.
        #[test]
        fn todo_lo_colocado_cumple_su_minimo(
            arbol in arbol_arbitrario(), w in 1u16..200, h in 1u16..80
        ) {
            // Con ids repetidos `kind_of` devuelve el del PRIMERO, así que el
            // mínimo que compararíamos podría no ser el de este hueco.
            prop_assume!(arbol.duplicate_slot_ids().is_empty());
            let out = resolve(Rect::new(0, 0, w, h), &arbol, &reg());
            if out.placements.len() > 1 {
                for (id, a) in &out.placements {
                    let kind = arbol.kind_of(*id).expect("colocado luego está");
                    let (mw, mh) = reg().min_of(kind);
                    prop_assert!(a.width >= mw && a.height >= mh, "{id:?} {a:?} < ({mw},{mh})");
                }
            }
        }

        /// `placements` y `hidden` PARTICIONAN los huecos del árbol. Si esto
        /// falla, un hueco vivo queda sin pintar Y sin suspender — con su
        /// watch abierto y nadie mirándolo.
        #[test]
        fn placements_y_hidden_particionan_el_arbol(
            arbol in arbol_arbitrario(), w in 1u16..200, h in 1u16..80
        ) {
            let out = resolve(Rect::new(0, 0, w, h), &arbol, &reg());
            let mut vistos: Vec<SlotId> = out.placements.iter().map(|(id, _)| *id).collect();
            vistos.extend(out.hidden.iter().copied());
            vistos.sort_unstable();
            let mut todos = arbol.slot_ids();
            todos.sort_unstable();
            prop_assert_eq!(vistos, todos);
        }

        /// `focus_order` solo lleva colocados y enfocables, sin repetir.
        #[test]
        fn el_orden_de_foco_solo_lleva_visibles_enfocables(
            arbol in arbol_arbitrario(), w in 1u16..200, h in 1u16..80
        ) {
            prop_assume!(arbol.duplicate_slot_ids().is_empty());
            let out = resolve(Rect::new(0, 0, w, h), &arbol, &reg());
            let colocados: Vec<SlotId> = out.placements.iter().map(|(id, _)| *id).collect();
            for id in &out.focus_order {
                prop_assert!(colocados.contains(id));
                let kind = arbol.kind_of(*id).expect("enfocable luego está");
                prop_assert!(reg().get(kind).is_some_and(|d| d.focusable));
            }
        }

        /// Nunca una pantalla vacía: si hay un hueco, se pinta alguno.
        #[test]
        fn siempre_se_pinta_algo(
            arbol in arbol_arbitrario(), w in 1u16..200, h in 1u16..80
        ) {
            let out = resolve(Rect::new(0, 0, w, h), &arbol, &reg());
            prop_assert!(!out.placements.is_empty());
        }
    }
}
