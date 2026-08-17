//! El árbol: tres tipos de nodo y los tipos que los identifican.
//!
//! Es el formato ÚNICO — el fichero de layout, el blob de sesión de L2 y lo
//! que escupirá el editor de layouts son esto mismo. Dos formatos obligarían a
//! migrar entre ellos, que es justo lo que la ADR 0058 evita.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Un rectángulo en CELDAS.
///
/// Propio y no el de ratatui: este crate no depende de ningún toolkit, y la
/// GUI escala estas celdas por su métrica de fuente. Los campos se llaman
/// igual que los de `ratatui::layout::Rect` a propósito, para que la
/// conversión en el TUI sea campo a campo y sin interpretación que hacer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    /// Columna de la esquina superior izquierda.
    pub x: u16,
    /// Fila de la esquina superior izquierda.
    pub y: u16,
    /// Ancho en celdas.
    pub width: u16,
    /// Alto en celdas.
    pub height: u16,
}

impl Rect {
    /// Atajo de construcción, muy usado por los tests y por el reparto.
    #[must_use]
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// Identidad de un hueco.
///
/// Se acuña por layout y NO se reutiliza dentro de una sesión: cerrar un hueco
/// deja su estado huérfano en el [`crate::layout::SlotStore`], para que
/// reabrir la misma disposición recupere el historial en vez de arrancar en
/// blanco.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SlotId(pub u32);

/// Qué hay dentro de un hueco.
///
/// STRING y no enum: un enum cierra el registro, y con él la puerta a que un
/// plugin aporte un kind (ADR 0058 D2). Un kind que este binario no conoce no
/// es un error — se pinta como una caja con su nombre y sus `params` se
/// conservan al reserializar.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct KindId(String);

impl KindId {
    /// Un kind cualquiera, por nombre.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// El kind del listado de ficheros.
    #[must_use]
    pub fn browser() -> Self {
        Self::new("browser")
    }

    /// El nombre, para la tabla de renderers de cada frontend.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Dirección de un [`Node::Split`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dir {
    /// Los hijos se reparten el ANCHO, uno al lado del otro.
    Horizontal,
    /// Los hijos se reparten el ALTO, uno encima del otro.
    Vertical,
}

/// Cuánto sitio pide un hijo de un [`Node::Split`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Size {
    /// Tantas celdas, pase lo que pase. La barra de estado es `Fixed(1)`.
    ///
    /// **Gana al mínimo del kind**: si pides tres celdas para algo cuyo mínimo
    /// son cinco, te dan tres. El mínimo decide cuándo colapsa un reparto
    /// PROPORCIONAL; no desautoriza una orden explícita.
    Fixed(u16),
    /// Reparto proporcional de lo que sobre tras los fijos. Los panes.
    Weight(u16),
    /// Lo que pida su contenido.
    ///
    /// La franja de tareas mide `min(tareas, 6)` filas y vale CERO en reposo,
    /// y eso solo lo sabe quien tiene el `TaskBoard` delante. Se sustituye por
    /// un [`Size::Fixed`] con [`Node::substitute_auto`] ANTES de repartir, así
    /// que `resolve` nunca lo ve y sigue siendo pura.
    Auto,
}

/// Parámetros de un hueco: bolsa OPACA que solo interpreta su kind.
///
/// El motor no la lee nunca — es la mitad cliente de la misma decisión que
/// impide al core leerla (ADR 0058 D4). Para un `browser` lleva el directorio
/// de arranque; para un futuro `preview`, el modo de ajuste de línea.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Params(serde_json::Map<String, serde_json::Value>);

impl Params {
    /// Una bolsa vacía.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// El valor de una clave, si está.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.0.get(key)
    }

    /// Pone una clave. La usa cada kind con las suyas; el motor jamás.
    pub fn set(&mut self, key: impl Into<String>, value: serde_json::Value) {
        self.0.insert(key.into(), value);
    }

    /// ¿Sin ningún parámetro?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Un puntero con nombre dentro del árbol, resuelto en cada frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleId {
    /// El hueco con el foco.
    Active,
    /// A dónde va una operación que necesita un segundo sitio.
    Target,
}

/// A quién sigue un hueco.
///
/// Sin esto, un panel auxiliar es una caja sin nada dentro: un `metadata` que
/// no sabe de quién enseñar el cursor no enseña nada.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Follow {
    /// Sigue a quien tenga ese rol AHORA. `Role(Active)` es el default útil.
    Role(RoleId),
    /// Sigue a un hueco concreto, pase lo que pase con el foco.
    Slot(SlotId),
}

/// Los vínculos de un hueco.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bindings {
    /// A quién mira este hueco. `None` = a nadie (un `browser` no mira a
    /// nadie: es él quien es mirado).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follows: Option<Follow>,
}

/// Un nodo del árbol.
///
/// Tres variantes y ni una más: **las pestañas son un TIPO DE NODO, no una
/// feature**, así que dónde caen decide si son espacios de trabajo, pestañas
/// de panel o media pantalla alternando vistas (ADR 0058 D1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Node {
    /// Los hijos se reparten el área a lo largo de `dir`, según `sizes`.
    Split {
        /// Por dónde se corta.
        dir: Dir,
        /// Los hijos, en orden de pintado.
        children: Vec<Node>,
        /// Cuánto pide cada hijo. Índice-paralelo a `children`.
        sizes: Vec<Size>,
    },
    /// Los hijos ocupan el mismo área y solo uno se ve.
    Tabs {
        /// Las pestañas, en orden.
        children: Vec<Node>,
        /// Cuál se ve. Fuera de rango se clampa con diagnóstico.
        active: usize,
    },
    /// Una hoja: un panel.
    Slot {
        /// Su identidad, estable mientras el layout no la borre.
        id: SlotId,
        /// Qué panel es.
        kind: KindId,
        /// Lo que ese panel necesite. El motor no lo lee.
        #[serde(default, skip_serializing_if = "Params::is_empty")]
        params: Params,
        /// A quién mira.
        #[serde(default)]
        bindings: Bindings,
    },
}

impl Node {
    /// Un hueco sin params ni vínculos.
    #[must_use]
    pub fn slot(id: SlotId, kind: KindId) -> Self {
        Self::Slot {
            id,
            kind,
            params: Params::new(),
            bindings: Bindings::default(),
        }
    }

    /// Todos los ids del árbol en orden de lectura, INCLUIDOS los de pestañas
    /// no activas: un hueco oculto sigue existiendo y sigue teniendo estado.
    #[must_use]
    pub fn slot_ids(&self) -> Vec<SlotId> {
        let mut out = Vec::new();
        self.collect_slot_ids(&mut out);
        out
    }

    fn collect_slot_ids(&self, out: &mut Vec<SlotId>) {
        match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => {
                for c in children {
                    c.collect_slot_ids(out);
                }
            }
            Self::Slot { id, .. } => out.push(*id),
        }
    }

    /// Un `Split` de hijos con el mismo peso. El caso corriente.
    #[must_use]
    pub fn split(dir: Dir, children: Vec<Node>) -> Self {
        let sizes = vec![Size::Weight(1); children.len()];
        Self::Split {
            dir,
            children,
            sizes,
        }
    }

    /// El primer hueco del subárbol en orden de lectura.
    ///
    /// Es a quien se le pregunta su tamaño natural: un `Auto` sobre un
    /// subárbol entero no tiene más remedio que apoyarse en alguien, y el
    /// primero es el único que no depende de cómo se reparta después.
    #[must_use]
    pub fn first_slot_id(&self) -> Option<SlotId> {
        match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => {
                children.iter().find_map(Self::first_slot_id)
            }
            Self::Slot { id, .. } => Some(*id),
        }
    }

    /// El mismo árbol con cada [`Size::Auto`] sustituido por el [`Size::Fixed`]
    /// que diga `natural` para el primer hueco de ese hijo.
    ///
    /// El árbol GUARDADO conserva sus `Auto`; el árbol del FRAME no los tiene.
    /// Así `resolve` no necesita una closure en su firma —que todos sus tests
    /// tendrían que pasar— y esta función se prueba sola.
    #[must_use]
    pub fn substitute_auto(&self, natural: &dyn Fn(SlotId) -> (u16, u16)) -> Self {
        match self {
            Self::Split {
                dir,
                children,
                sizes,
            } => {
                let hijos: Vec<Self> = children
                    .iter()
                    .map(|c| c.substitute_auto(natural))
                    .collect();
                let nuevos = children
                    .iter()
                    .enumerate()
                    .map(|(i, c)| match sizes.get(i) {
                        Some(Size::Auto) => {
                            let (w, h) = c.first_slot_id().map_or((0, 0), natural);
                            Size::Fixed(match dir {
                                Dir::Horizontal => w,
                                Dir::Vertical => h,
                            })
                        }
                        Some(otro) => *otro,
                        None => Size::Weight(1),
                    })
                    .collect();
                Self::Split {
                    dir: *dir,
                    children: hijos,
                    sizes: nuevos,
                }
            }
            Self::Tabs { children, active } => Self::Tabs {
                children: children
                    .iter()
                    .map(|c| c.substitute_auto(natural))
                    .collect(),
                active: *active,
            },
            Self::Slot { .. } => self.clone(),
        }
    }

    /// El kind del hueco `id`, si el árbol lo contiene.
    #[must_use]
    pub fn kind_of(&self, id: SlotId) -> Option<&KindId> {
        match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => {
                children.iter().find_map(|c| c.kind_of(id))
            }
            Self::Slot { id: this, kind, .. } if *this == id => Some(kind),
            Self::Slot { .. } => None,
        }
    }

    /// Los vínculos del hueco `id`, si el árbol lo contiene.
    #[must_use]
    pub fn bindings_of(&self, id: SlotId) -> Option<&Bindings> {
        match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => {
                children.iter().find_map(|c| c.bindings_of(id))
            }
            Self::Slot {
                id: this, bindings, ..
            } if *this == id => Some(bindings),
            Self::Slot { .. } => None,
        }
    }

    /// Los ids repetidos, si los hay. Un layout con dos huecos del mismo id es
    /// incoherente y NO se adivina cuál gana.
    #[must_use]
    pub fn duplicate_slot_ids(&self) -> Vec<SlotId> {
        let mut cuenta: BTreeMap<SlotId, usize> = BTreeMap::new();
        for id in self.slot_ids() {
            *cuenta.entry(id).or_default() += 1;
        }
        cuenta
            .into_iter()
            .filter_map(|(id, n)| (n > 1).then_some(id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El árbol hace round-trip: es el MISMO formato que el fichero de config,
    /// el blob de sesión de L2 y lo que escupirá el editor de layouts. Un
    /// formato que no round-trippea obliga a migrar entre dos, que es justo lo
    /// que la ADR 0058 evita.
    #[test]
    fn el_arbol_hace_round_trip() {
        let arbol = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::Tabs {
                    active: 1,
                    children: vec![
                        Node::slot(SlotId(2), KindId::browser()),
                        Node::slot(SlotId(3), KindId::new("viewer")),
                    ],
                },
            ],
        };
        let json = serde_json::to_string(&arbol).expect("serializa");
        assert_eq!(serde_json::from_str::<Node>(&json).expect("vuelve"), arbol);
    }

    /// Un kind DESCONOCIDO sobrevive al round-trip con sus `params` intactos.
    /// Es la regla 3 del modelo: un cliente que no sabe pintar un kind no puede
    /// borrárselo del layout al otro.
    #[test]
    fn un_kind_desconocido_conserva_sus_params() {
        let json = r#"{"slot":{"id":7,"kind":"terminal","params":{"shell":"fish"},"bindings":{}}}"#;
        let n: Node = serde_json::from_str(json).expect("un kind que no conocemos parsea");
        let vuelta = serde_json::to_string(&n).expect("serializa");
        assert!(
            vuelta.contains("\"shell\":\"fish\""),
            "los params se pierden: {vuelta}"
        );
    }

    /// Los ids visibles de un árbol, en orden de lectura. Lo usan roles, store
    /// y resolve, así que se prueba aquí una vez.
    #[test]
    fn slot_ids_recorre_tambien_las_pestanas_ocultas() {
        let arbol = Node::Tabs {
            active: 0,
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        };
        assert_eq!(arbol.slot_ids(), vec![SlotId(1), SlotId(2)]);
    }

    /// `substitute_auto` cambia los `Auto` por `Fixed` y NO toca nada más: el
    /// árbol guardado conserva sus `Auto`, el del frame no los tiene.
    #[test]
    fn substitute_auto_solo_cambia_los_auto() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Auto, Size::Fixed(1)],
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::new("tasks")),
                Node::slot(SlotId(3), KindId::new("status")),
            ],
        };
        let del_frame = arbol.substitute_auto(&|id| if id == SlotId(2) { (0, 4) } else { (0, 0) });
        let Node::Split { sizes, .. } = &del_frame else {
            panic!("sigue siendo un split")
        };
        assert_eq!(
            *sizes,
            vec![Size::Weight(1), Size::Fixed(4), Size::Fixed(1)]
        );
        let Node::Split { sizes: orig, .. } = &arbol else {
            panic!("split")
        };
        assert_eq!(orig[1], Size::Auto, "el árbol guardado no se toca");
    }

    /// En un corte HORIZONTAL, `Auto` toma el ANCHO natural, no el alto. Una
    /// sidebar mide lo que mide de ancha; su alto lo pone el reparto.
    #[test]
    fn substitute_auto_toma_el_eje_del_corte() {
        let arbol = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Auto, Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(1), KindId::new("places")),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        };
        let del_frame = arbol.substitute_auto(&|_| (18, 3));
        let Node::Split { sizes, .. } = &del_frame else {
            panic!("split")
        };
        assert_eq!(sizes[0], Size::Fixed(18), "el ancho, no el alto");
    }

    /// El `Auto` de un subárbol se apoya en su PRIMER hueco: es el único que
    /// no depende de cómo se reparta después.
    #[test]
    fn el_primer_hueco_es_a_quien_se_le_pregunta() {
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(7), KindId::browser()),
                Node::slot(SlotId(8), KindId::browser()),
            ],
        );
        assert_eq!(arbol.first_slot_id(), Some(SlotId(7)));
    }

    /// Dos huecos con el mismo id es incoherente, y el árbol sabe decirlo.
    #[test]
    fn los_ids_repetidos_se_detectan() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(1), KindId::browser()),
            ],
        };
        assert_eq!(arbol.duplicate_slot_ids(), vec![SlotId(1)]);
    }
}
