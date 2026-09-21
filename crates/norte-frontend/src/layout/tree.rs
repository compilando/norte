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

/// Un borde contra el que se acopla un panel.
///
/// Existe para [`Node::dock`]: un sidebar no se «parte» de un hueco (eso es
/// [`Node::split_slot`], que reparte el sitio de UNO), se pega al costado de
/// lo que ya hay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    /// Izquierda: primer hijo de un `Split` horizontal.
    Left,
    /// Derecha: último hijo de un `Split` horizontal.
    Right,
    /// Arriba: primer hijo de un `Split` vertical.
    Top,
    /// Abajo: último hijo de un `Split` vertical.
    Bottom,
}

impl Edge {
    /// El eje en el que corta este borde.
    #[must_use]
    pub const fn axis(self) -> Dir {
        match self {
            Self::Left | Self::Right => Dir::Horizontal,
            Self::Top | Self::Bottom => Dir::Vertical,
        }
    }

    /// ¿Va DELANTE de los que ya están?
    #[must_use]
    pub const fn is_front(self) -> bool {
        matches!(self, Self::Left | Self::Top)
    }
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

impl Bindings {
    /// ¿No vincula nada? Lo usa la serialización para no escribir una tabla
    /// vacía por cada hueco: la mayoría de los huecos no miran a nadie, y un
    /// `[...slot.bindings]` sin contenido es ruido en el fichero que un
    /// usuario copia y bytes en el cuerpo de la sesión.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.follows.is_none()
    }
}

/// Cómo queda una [`Node::Tabs`] tras una operación: sus hijos nuevos y cuál
/// queda activa. Recibe los hijos de ahora y la posición del que se opera.
type ReTab<'a> = dyn Fn(&[Node], usize) -> (Vec<Node>, usize) + 'a;

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
        #[serde(default, skip_serializing_if = "Bindings::is_empty")]
        bindings: Bindings,
    },
}

/// Una hoja que es un PANEL: ni un listado ni una fila de cromo.
fn es_panel(n: &Node) -> bool {
    matches!(n, Node::Slot { kind, .. }
        if !matches!(kind.as_str(), "browser" | "status" | "tasks"))
}

/// Una fila de CROMO: la barra de estado o la franja de tareas. No se
/// mueven ni reciben, y un reparto que las contiene no se gira: la barra de
/// estado de lado dejaría de ser una barra (ADR 0138).
fn es_cromo(n: &Node) -> bool {
    matches!(n, Node::Slot { kind, .. } if matches!(kind.as_str(), "status" | "tasks"))
}

/// Lo que devuelve buscar qué girar (ADR 0138): tres casos y no un
/// `Option`, porque «aquí no se gira» tiene que PARAR la búsqueda y «aquí
/// no está» tiene que dejarla seguir. Con un `Option` la negativa subía y
/// se giraba el reparto de fuera.
enum Giro {
    /// Girado: el árbol nuevo.
    Hecho(Node),
    /// Encontrado, y no se gira.
    Rehusado,
    /// El hueco no está en este subárbol, o no hay reparto que girar.
    NoEsta,
}

/// Dónde cae un hueco que se suelta sobre otro (ADR 0138): a uno de sus
/// cuatro lados, o en el CENTRO, que es unirse a él como pestaña.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropZone {
    /// A la izquierda del destino.
    Left,
    /// A la derecha.
    Right,
    /// Encima.
    Top,
    /// Debajo.
    Bottom,
    /// Como pestaña del destino.
    Center,
}

impl DropZone {
    /// La zona de un rectángulo bajo el punto `(x, y)`: el lado más cercano
    /// si está a menos de un CUARTO de él, y si no el centro. La misma regla
    /// que `zonaDe` en la ventana (`render/mover.ts`).
    #[must_use]
    pub fn at(x: u16, y: u16, rect: Rect) -> Self {
        let frac = |p: u16, o: u16, largo: u16| {
            if largo == 0 {
                0.5
            } else {
                (f32::from(p.saturating_sub(o)) + 0.5) / f32::from(largo)
            }
        };
        let fx = frac(x, rect.x, rect.width);
        let fy = frac(y, rect.y, rect.height);
        let lados = [
            (Self::Left, fx),
            (Self::Right, 1.0 - fx),
            (Self::Top, fy),
            (Self::Bottom, 1.0 - fy),
        ];
        let (lado, d) =
            lados.into_iter().fold(
                (Self::Center, f32::INFINITY),
                |m, l| if l.1 < m.1 { l } else { m },
            );
        if d < 0.25 { lado } else { Self::Center }
    }

    /// La parte de `rect` que ocupa la zona: una mitad, o el rectángulo
    /// entero para el centro. Es lo que se resalta mientras se arrastra.
    #[must_use]
    pub fn part_of(self, rect: Rect) -> Rect {
        let (w2, h2) = (rect.width / 2, rect.height / 2);
        match self {
            Self::Left => Rect { width: w2, ..rect },
            Self::Right => Rect {
                x: rect.x + w2,
                width: rect.width - w2,
                ..rect
            },
            Self::Top => Rect { height: h2, ..rect },
            Self::Bottom => Rect {
                y: rect.y + h2,
                height: rect.height - h2,
                ..rect
            },
            Self::Center => rect,
        }
    }

    /// El borde de la zona; `None` para el centro.
    #[must_use]
    pub const fn edge(self) -> Option<Edge> {
        match self {
            Self::Left => Some(Edge::Left),
            Self::Right => Some(Edge::Right),
            Self::Top => Some(Edge::Top),
            Self::Bottom => Some(Edge::Bottom),
            Self::Center => None,
        }
    }
}

/// Un panel suelto, o un grupo de pestañas hecho SOLO de paneles: lo que un
/// panel nuevo del mismo borde puede unirse (fase F). Un grupo con un
/// listado dentro es el de las pestañas de un listado, y ahí no se mete un
/// panel.
fn es_grupo_de_paneles(n: &Node) -> bool {
    match n {
        Node::Tabs { children, .. } => !children.is_empty() && children.iter().all(es_panel),
        otro => es_panel(otro),
    }
}

/// El tamaño con que entra un hijo nuevo en un reparto que ya tiene
/// `hermanos`.
///
/// Un [`Size::Weight`] es una PROPORCIÓN, así que solo significa algo al
/// lado de los otros pesos: `Weight(1)` quiere decir «como uno de ellos» en
/// un reparto de unos, pero arrastrar un borde deja a los listados en 49/51 y
/// entonces el mismo `Weight(1)` es un píxel. Se escala por la MEDIA de los
/// pesos hermanos. Un [`Size::Fixed`] o un reparto sin pesos no cambian.
fn peso_entre_hermanos(size: Size, hermanos: &[Size]) -> Size {
    let Size::Weight(w) = size else {
        return size;
    };
    let pesos: Vec<u32> = hermanos
        .iter()
        .filter_map(|s| match s {
            Size::Weight(p) => Some(u32::from(*p)),
            _ => None,
        })
        .collect();
    if pesos.is_empty() {
        return size;
    }
    let n = u32::try_from(pesos.len()).unwrap_or(u32::MAX);
    let media = (pesos.iter().sum::<u32>() + n / 2) / n;
    Size::Weight(u16::try_from(media.saturating_mul(u32::from(w)).max(1)).unwrap_or(u16::MAX))
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

    /// Un hueco con vínculos: de quién es vista.
    ///
    /// Lo pide el preview acoplado, que es el kind `viewer` de siempre con un
    /// `follows` puesto — el kind dice QUÉ hay dentro y el vínculo dice de
    /// quién es vista, que es justo la separación del ADR 0058.
    #[must_use]
    pub fn slot_bound(id: SlotId, kind: KindId, bindings: Bindings) -> Self {
        Self::Slot {
            id,
            kind,
            params: Params::new(),
            bindings,
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

    /// Parte el hueco `id` en dos a lo largo de `dir`, con `nuevo` al lado.
    ///
    /// Los dos quedan con el mismo peso. Si `id` está dentro de una `Tabs`, el
    /// corte va DENTRO de esa pestaña y no alrededor del grupo: partir una
    /// pestaña es partir lo que estás mirando, no reorganizar sus hermanas.
    ///
    /// # Partir otra vez en el mismo eje APLANA
    ///
    /// Si el hueco ya vive en un `Split` que corre en `dir`, el nuevo entra
    /// como hermano suyo en vez de envolverlo en otro `Split`. Anidando, cada
    /// partición se llevaba la mitad de la mitad: tres paneles quedaban en
    /// 1/2, 1/4 y 1/4 en vez de tercios, y a la siguiente el hijo más profundo
    /// bajaba del mínimo de su kind y el reparto lo degradaba a pestañas — el
    /// panel recién pedido desaparecía de la pantalla sin decir nada, con el
    /// árbol guardándolo igualmente.
    ///
    /// Solo si el hueco es PONDERADO. Uno de tamaño fijo es cromo acoplado:
    /// meter otro hijo en su fila le robaría el sitio a lo que tiene al lado,
    /// así que ese se parte por dentro, como siempre. El nuevo nace con el
    /// mismo peso que aquel del que sale, que sobre el reparto por defecto
    /// —todos a uno— es exactamente repartir a partes iguales.
    #[must_use]
    pub fn split_slot(&self, id: SlotId, dir: Dir, nuevo: &Self) -> Self {
        match self {
            Self::Slot { id: i, .. } if *i == id => {
                Self::split(dir, vec![self.clone(), nuevo.clone()])
            }
            Self::Slot { .. } => self.clone(),
            Self::Split {
                dir: d,
                children,
                sizes,
            } => {
                if *d == dir
                    && let Some(i) = children
                        .iter()
                        .position(|c| matches!(c, Self::Slot { id: s, .. } if *s == id))
                    && let Size::Weight(peso) = sizes.get(i).copied().unwrap_or(Size::Weight(1))
                {
                    let mut hijos = children.clone();
                    let mut tam = sizes.clone();
                    tam.resize(hijos.len(), Size::Weight(1));
                    hijos.insert(i + 1, nuevo.clone());
                    tam.insert(i + 1, Size::Weight(peso));
                    return Self::Split {
                        dir: *d,
                        children: hijos,
                        sizes: tam,
                    };
                }
                Self::Split {
                    dir: *d,
                    sizes: sizes.clone(),
                    children: children
                        .iter()
                        .map(|c| c.split_slot(id, dir, nuevo))
                        .collect(),
                }
            }
            Self::Tabs { children, active } => Self::Tabs {
                active: *active,
                children: children
                    .iter()
                    .map(|c| c.split_slot(id, dir, nuevo))
                    .collect(),
            },
        }
    }

    /// Acopla `nuevo` contra el borde `edge` del reparto donde vive `anchor`.
    ///
    /// El sitio exacto es el `Split` MÁS PROFUNDO que contiene a `anchor` y
    /// corre en el eje de `edge`; ahí entra como primer hijo (`Left`/`Top`) o
    /// como último (`Right`/`Bottom`), con el tamaño `size`.
    ///
    /// Buscar ese split y no la raíz es la diferencia entre un sidebar al lado
    /// de los listados y un sidebar al lado de TODO: en el preset `orthodox`
    /// la raíz es vertical (cuerpo, tareas, barra de estado), así que envolver
    /// la raíz dejaría la barra de estado y la franja de tareas a la derecha
    /// del sidebar en vez de debajo de los listados.
    ///
    /// Si ningún ancestro corre en ese eje —un solo panel, o una pila
    /// vertical— se envuelve el árbol entero en un `Split` nuevo, con lo que
    /// había ponderado. Un `anchor` que no está devuelve el árbol intacto.
    ///
    /// Lo contrario es [`Self::close_slot`], que ya disuelve el `Split` que se
    /// queda con un hijo: acoplar y desacoplar devuelve el árbol de partida.
    #[must_use]
    pub fn dock(&self, anchor: SlotId, edge: Edge, size: Size, nuevo: &Self) -> Self {
        self.dock_con(anchor, edge, size, nuevo, false)
    }

    /// Como [`Self::dock`], pero un PANEL que llega a un borde donde ya hay
    /// un panel —o un grupo de paneles— se une a él como pestaña, delante, en
    /// vez de abrir otra columna o fila (spec 2026-09-21, fase F).
    ///
    /// Es lo que hace VS Code: las vistas de un mismo borde comparten sitio
    /// y la barra de actividad elige cuál se ve. Con `dock` a secas, cuatro
    /// paneles a la derecha eran cuatro columnas de treinta celdas y los
    /// listados se quedaban con lo que sobrase. El grupo conserva su tamaño;
    /// cerrar una pestaña de un grupo de dos lo deshace ([`Self::close_slot`]).
    ///
    /// Un listado nunca se agrupa, ni hace de grupo: dos listados lado a lado
    /// son el gestor ortodoxo.
    #[must_use]
    pub fn dock_grouped(&self, anchor: SlotId, edge: Edge, size: Size, nuevo: &Self) -> Self {
        self.dock_con(anchor, edge, size, nuevo, true)
    }

    fn dock_con(
        &self,
        anchor: SlotId,
        edge: Edge,
        size: Size,
        nuevo: &Self,
        agrupar: bool,
    ) -> Self {
        if !self.contains(anchor) {
            return self.clone();
        }
        self.dock_inner(anchor, edge, size, nuevo, agrupar)
            .unwrap_or_else(|| {
                let (children, sizes) = if edge.is_front() {
                    (
                        vec![nuevo.clone(), self.clone()],
                        vec![size, Size::Weight(1)],
                    )
                } else {
                    (
                        vec![self.clone(), nuevo.clone()],
                        vec![Size::Weight(1), size],
                    )
                };
                Self::Split {
                    dir: edge.axis(),
                    children,
                    sizes,
                }
            })
    }

    /// `Some` si algún `Split` del camino a `anchor` corría en el eje pedido y
    /// se quedó con `nuevo`; `None` si ninguno, y entonces decide [`Self::dock`].
    fn dock_inner(
        &self,
        anchor: SlotId,
        edge: Edge,
        size: Size,
        nuevo: &Self,
        agrupar: bool,
    ) -> Option<Self> {
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        let pos = hijos.iter().position(|c| c.contains(anchor))?;
        // Primero hacia dentro: el reparto que manda es el más PROFUNDO que
        // corre en el eje, no el primero que se encuentra bajando.
        if let Some(dentro) = hijos[pos].dock_inner(anchor, edge, size, nuevo, agrupar) {
            return Some(self.with_child(pos, dentro));
        }
        // Una `Tabs` no acepta el acople: meterlo dentro de una pestaña haría
        // que el sidebar desapareciera al cambiar de pestaña, que es justo lo
        // que un sidebar no hace. Sube al padre.
        let Self::Split {
            dir,
            children,
            sizes,
        } = self
        else {
            return None;
        };
        if *dir != edge.axis() {
            return None;
        }
        let mut nc = children.clone();
        let mut ns = sizes.clone();
        // Por detrás, pero por DELANTE de las filas de cromo del final (la
        // franja de tareas y la barra de estado): un panel acoplado abajo
        // va encima de la barra de estado, como en VS Code, y en el
        // terminal la barra tiene que seguir siendo la última fila.
        let at = if edge.is_front() {
            0
        } else {
            nc.len()
                - nc.iter()
                    .rev()
                    .take_while(|c| {
                        matches!(c, Self::Slot { kind, .. }
                            if kind.as_str() == "status" || kind.as_str() == "tasks")
                    })
                    .count()
        };
        // Fase F: si en ese borde ya hay un PANEL (o un grupo de paneles),
        // el nuevo se une a él como pestaña, delante.
        let vecino = if edge.is_front() {
            Some(0)
        } else {
            at.checked_sub(1)
        };
        if agrupar
            && es_panel(nuevo)
            && let Some(v) = vecino
            && nc.get(v).is_some_and(es_grupo_de_paneles)
        {
            let grupo = match &nc[v] {
                Self::Tabs { children, .. } => {
                    let mut h = children.clone();
                    h.push(nuevo.clone());
                    h
                }
                otro => vec![otro.clone(), nuevo.clone()],
            };
            let active = grupo.len() - 1;
            nc[v] = Self::Tabs {
                children: grupo,
                active,
            };
            // El sitio del grupo es el MAYOR de los que piden sus paneles:
            // los detalles fijos a treinta y el visor detrás no pueden dejar
            // al visor en treinta columnas. Un peso gana a un fijo (el que
            // pide sitio proporcional es el que más necesita).
            if let Some(actual) = ns.get(v).copied() {
                ns[v] = match (actual, size) {
                    (Size::Fixed(a), Size::Fixed(b)) => Size::Fixed(a.max(b)),
                    (Size::Fixed(_), Size::Weight(_)) => peso_entre_hermanos(size, sizes),
                    _ => actual,
                };
            }
            return Some(Self::Split {
                dir: *dir,
                children: nc,
                sizes: ns,
            });
        }
        nc.insert(at, nuevo.clone());
        ns.insert(at.min(ns.len()), peso_entre_hermanos(size, sizes));
        Some(Self::Split {
            dir: *dir,
            children: nc,
            sizes: ns,
        })
    }

    /// Cierra el hueco `id`: lo saca de su padre.
    ///
    /// Un `Split` o una `Tabs` que se queda con UN hijo se disuelve en él.
    /// Devuelve `None` si `id` es la raíz o no está: cerrar el último panel
    /// dejaría una pantalla sin nada, y eso lo decide el llamante.
    #[must_use]
    pub fn close_slot(&self, id: SlotId) -> Option<Self> {
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        for (i, c) in hijos.iter().enumerate() {
            if let Some(cambiado) = c.close_slot(id) {
                return Some(self.with_child(i, cambiado));
            }
        }
        let pos = hijos.iter().position(|c| c.contains(id))?;
        if hijos.len() <= 1 {
            return None;
        }
        match self {
            Self::Split {
                dir,
                children,
                sizes,
            } => {
                let mut nc = children.clone();
                let mut ns = sizes.clone();
                nc.remove(pos);
                if pos < ns.len() {
                    ns.remove(pos);
                }
                if nc.len() == 1 {
                    return nc.into_iter().next();
                }
                Some(Self::Split {
                    dir: *dir,
                    children: nc,
                    sizes: ns,
                })
            }
            Self::Tabs { .. } => self.close_tab(id),
            Self::Slot { .. } => None,
        }
    }

    /// Mueve el hueco `id` junto a `target`: a su lado `zona`, o como
    /// pestaña suya si la zona es el centro (ADR 0138). Es arrastrar un
    /// panel por su título y soltarlo sobre otro, como en VS Code.
    ///
    /// La UNIDAD de destino es el hueco, o el grupo de pestañas donde vive:
    /// soltar a la derecha de una pestaña parte el grupo entero, no la mete
    /// dentro de él. Si el padre de esa unidad ya reparte en el eje de la
    /// zona y la unidad es ponderada, el hueco entra como hermano con el
    /// mismo peso —tres listados quedan en tercios, no en 1/2, 1/4, 1/4—; si
    /// no, la unidad se envuelve en un reparto nuevo a partes iguales, y un
    /// panel de ancho fijo conserva su ancho por fuera.
    ///
    /// No hace nada —devuelve el árbol tal cual— si `id` y `target` son el
    /// mismo, si alguno falta o es cromo (estado, tareas), o si `id` es el
    /// único hueco. Nada se crea ni se pierde: el hueco movido conserva su
    /// id, su kind, sus parámetros y sus vínculos.
    #[must_use]
    pub fn move_slot(&self, id: SlotId, target: SlotId, zona: DropZone) -> Self {
        let movible = |s: SlotId| self.find_slot(s).is_some_and(|n| !es_cromo(n));
        if id == target || !movible(id) || !movible(target) {
            return self.clone();
        }
        let Some(nodo) = self.find_slot(id).cloned() else {
            return self.clone();
        };
        // El CENTRO solo junta lo que ya es de la misma familia: un listado
        // con listados, un panel con paneles (ADR 0134). Un listado metido
        // en las pestañas de los sitios viviría en dieciséis columnas, y un
        // grupo mezclado dejaría de ser un grupo de paneles para siempre.
        if zona == DropZone::Center
            && self
                .find_slot(target)
                .is_none_or(|t| es_panel(t) != es_panel(&nodo))
        {
            return self.clone();
        }
        let Some(resto) = self.close_slot(id) else {
            return self.clone();
        };
        match zona.edge() {
            None => resto.add_tab(target, &nodo),
            Some(edge) => resto
                .place_beside(target, edge, &nodo)
                .unwrap_or_else(|| self.clone()),
        }
    }

    /// El nodo HOJA del hueco `id`.
    fn find_slot(&self, id: SlotId) -> Option<&Self> {
        match self {
            Self::Slot { id: i, .. } if *i == id => Some(self),
            Self::Slot { .. } => None,
            Self::Split { children, .. } | Self::Tabs { children, .. } => {
                children.iter().find_map(|c| c.find_slot(id))
            }
        }
    }

    /// ¿Es este nodo la unidad que representa a `target` en su padre: el
    /// propio hueco, o el grupo de pestañas del que es hijo directo?
    fn es_unidad_de(&self, target: SlotId) -> bool {
        match self {
            Self::Slot { id, .. } => *id == target,
            Self::Tabs { children, .. } => children
                .iter()
                .any(|c| matches!(c, Self::Slot { id, .. } if *id == target)),
            Self::Split { .. } => false,
        }
    }

    /// `nodo` al lado `edge` de la unidad de `target`; `None` si no está.
    fn place_beside(&self, target: SlotId, edge: Edge, nodo: &Self) -> Option<Self> {
        if self.es_unidad_de(target) {
            let (children, sizes) = if edge.is_front() {
                (vec![nodo.clone(), self.clone()], vec![Size::Weight(1); 2])
            } else {
                (vec![self.clone(), nodo.clone()], vec![Size::Weight(1); 2])
            };
            return Some(Self::Split {
                dir: edge.axis(),
                children,
                sizes,
            });
        }
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        let pos = hijos.iter().position(|c| c.contains(target))?;
        // Hermano en el MISMO reparto, si corre en el eje y la unidad pesa:
        // así se parte a partes iguales, como `split_slot`. Nunca en el
        // reparto del cromo: ese no se gira, y lo que se soltara ahí ya no
        // se podría girar de vuelta con `layout.flip`.
        if let Self::Split {
            dir,
            children,
            sizes,
        } = self
            && *dir == edge.axis()
            && !children.iter().any(es_cromo)
            && children[pos].es_unidad_de(target)
        {
            // Junto a un panel de ancho FIJO tambien entra como hermano, con
            // peso: partirlo por dentro le daría la mitad de sus dieciséis
            // columnas a un listado.
            let tam = match sizes.get(pos).copied().unwrap_or(Size::Weight(1)) {
                Size::Weight(peso) => Size::Weight(peso),
                Size::Fixed(_) | Size::Auto => peso_entre_hermanos(Size::Weight(1), sizes),
            };
            let mut nc = children.clone();
            let mut ns = sizes.clone();
            ns.resize(nc.len(), Size::Weight(1));
            let at = if edge.is_front() { pos } else { pos + 1 };
            nc.insert(at, nodo.clone());
            ns.insert(at, tam);
            return Some(Self::Split {
                dir: *dir,
                children: nc,
                sizes: ns,
            });
        }
        let dentro = hijos[pos].place_beside(target, edge, nodo)?;
        Some(self.with_child(pos, dentro))
    }

    /// Gira el reparto más interior que contiene `id`: lado a lado pasa a
    /// uno encima del otro, y al revés (ADR 0138, `layout.flip`).
    ///
    /// Lo que se gira es la RACHA de hermanos ponderados que rodea al hueco:
    /// en `H[sitios Fijo(16), a, b]` se apilan `a` y `b` y los sitios siguen
    /// siendo una columna de dieciséis — girar la fila entera los habría
    /// convertido en una banda y el ancho no volvería al girar de nuevo. Si
    /// la racha es el reparto entero, se gira el reparto; si es solo una
    /// parte, esa parte pasa a un reparto propio, con el peso que sumaba.
    ///
    /// Un reparto con cromo dentro (barra de estado, tareas) no se gira —la
    /// barra de lado dejaría de ser una barra—, ni una racha de uno. Entonces
    /// no pasa NADA: la negativa no sube a probar con el reparto de fuera.
    #[must_use]
    pub fn flip(&self, id: SlotId) -> Self {
        match self.flip_inner(id) {
            Giro::Hecho(n) => n,
            Giro::Rehusado | Giro::NoEsta => self.clone(),
        }
    }

    fn flip_inner(&self, id: SlotId) -> Giro {
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return Giro::NoEsta,
        };
        let Some(pos) = hijos.iter().position(|c| c.contains(id)) else {
            return Giro::NoEsta;
        };
        match hijos[pos].flip_inner(id) {
            Giro::Hecho(dentro) => return Giro::Hecho(self.with_child(pos, dentro)),
            Giro::Rehusado => return Giro::Rehusado,
            Giro::NoEsta => {}
        }
        let Self::Split {
            dir,
            children,
            sizes,
        } = self
        else {
            return Giro::NoEsta;
        };
        if children.iter().any(es_cromo) {
            return Giro::Rehusado;
        }
        let otro = match dir {
            Dir::Horizontal => Dir::Vertical,
            Dir::Vertical => Dir::Horizontal,
        };
        let pesa = |i: usize| matches!(sizes.get(i), Some(Size::Weight(_)) | None);
        if !pesa(pos) {
            return Giro::Rehusado;
        }
        let mut desde = pos;
        while desde > 0 && pesa(desde - 1) {
            desde -= 1;
        }
        let mut hasta = pos + 1;
        while hasta < children.len() && pesa(hasta) {
            hasta += 1;
        }
        if hasta - desde < 2 {
            return Giro::Rehusado;
        }
        if desde == 0 && hasta == children.len() {
            return Giro::Hecho(Self::Split {
                dir: otro,
                children: children.clone(),
                sizes: sizes.clone(),
            });
        }
        let cuanto = |i: usize| match sizes.get(i) {
            Some(Size::Weight(w)) => u32::from(*w),
            _ => 1,
        };
        let total = u16::try_from((desde..hasta).map(cuanto).sum::<u32>()).unwrap_or(u16::MAX);
        let racha = Self::Split {
            dir: otro,
            children: children[desde..hasta].to_vec(),
            sizes: (desde..hasta)
                .map(|i| sizes.get(i).copied().unwrap_or(Size::Weight(1)))
                .collect(),
        };
        let mut nc = children[..desde].to_vec();
        nc.push(racha);
        nc.extend_from_slice(&children[hasta..]);
        let mut ns: Vec<Size> = (0..desde)
            .map(|i| sizes.get(i).copied().unwrap_or(Size::Weight(1)))
            .collect();
        ns.push(Size::Weight(total.max(1)));
        ns.extend(
            (hasta..children.len()).map(|i| sizes.get(i).copied().unwrap_or(Size::Weight(1))),
        );
        Giro::Hecho(Self::Split {
            dir: *dir,
            children: nc,
            sizes: ns,
        })
    }

    /// Cambia el tamaño del hijo que contiene `id` en `delta`.
    ///
    /// Un hijo PONDERADO se mueve de peso, entre 1 y 10. Un hijo FIJO se mueve
    /// en CELDAS, dos por pulsación, entre 2 y 100: un sidebar pidió un ancho
    /// concreto, y hasta #227 eso quería decir que el teclado no podía
    /// cambiarlo — que es un ancho impuesto, no un ancho elegido. El tope de
    /// abajo no es cosmético: a cero el panel desaparece y con él la forma de
    /// devolverlo.
    ///
    /// [`Size::Auto`] no se toca: se sustituye por un fijo ANTES de repartir,
    /// así que un número guardado aquí lo pisaría el siguiente frame y la
    /// tecla parecería rota.
    #[must_use]
    pub fn resize(&self, id: SlotId, delta: i16) -> Self {
        /// Celdas por pulsación en un hijo fijo.
        const PASO: i32 = 2;
        self.map_split_of(id, &|sizes, pos| {
            let mut ns = sizes.to_vec();
            match ns.get(pos) {
                Some(Size::Weight(w)) => {
                    let nuevo = i32::from(*w).saturating_add(i32::from(delta)).clamp(1, 10);
                    ns[pos] = Size::Weight(u16::try_from(nuevo).unwrap_or(1));
                }
                Some(Size::Fixed(n)) => {
                    let nuevo = i32::from(*n)
                        .saturating_add(i32::from(delta).saturating_mul(PASO))
                        .clamp(2, 100);
                    ns[pos] = Size::Fixed(u16::try_from(nuevo).unwrap_or(2));
                }
                Some(Size::Auto) | None => {}
            }
            ns
        })
    }

    /// Pone el borde ENTRE `id` y su hermano de la derecha (o de abajo) en la
    /// fracción `frac` del espacio que ocupan los dos juntos.
    ///
    /// Es la primitiva del ARRASTRE, y por eso es absoluta y no un paso:
    /// [`Self::resize`] mueve dos celdas por pulsación, que es lo que quiere
    /// una tecla; un ratón dice DÓNDE va el borde, y convertir eso en una
    /// ristra de pasos daría un borde que no llega a donde está el puntero.
    ///
    /// La suma de los dos tamaños se CONSERVA: lo que uno gana lo pierde el
    /// otro, y el resto de la fila no se entera. Un `Split` de cinco huecos
    /// donde arrastrar un borde recolocara los cinco sería un gesto que toca
    /// lo que nadie ha agarrado.
    ///
    /// Con pesos se renormaliza la pareja a cien (`PESO_FINO`) para que el
    /// arrastre tenga grano: dos huecos por defecto son `Weight(1)` y
    /// `Weight(1)`, y sobre esa pareja solo existiría la mitad exacta.
    ///
    /// `frac` se acota para que ninguno de los dos desaparezca: un hueco a
    /// cero se lleva con él la forma de devolverlo.
    ///
    /// [`Size::Auto`] no se toca, por lo mismo que en [`Self::resize`]: lo
    /// sustituye el reparto y un número guardado aquí lo pisaría el siguiente
    /// frame.
    /// `celdas_del_par` es lo que los dos ocupan juntos, en celdas de reparto.
    /// Lo sabe quien pinta, no el árbol: un [`Size::Fixed`] se mide en celdas
    /// y una fracción sola no basta para escribirlo.
    #[must_use]
    pub fn drag_border(&self, id: SlotId, frac: f32, celdas_del_par: u16) -> Self {
        /// Peso total al que se renormaliza una pareja arrastrada.
        const PESO_FINO: u16 = 100;
        /// Lo mínimo que le queda a cada lado, en tanto por uno.
        const MARGEN: f32 = 0.05;
        let frac = frac.clamp(MARGEN, 1.0 - MARGEN);
        self.map_split_of(id, &|sizes, pos| {
            let mut ns = sizes.to_vec();
            let Some(siguiente) = ns.get(pos + 1).copied() else {
                // El último no tiene borde a su derecha: el que se arrastra
                // es el suyo con el anterior, y quien llama nombra el hueco
                // de la IZQUIERDA del borde.
                return ns;
            };
            // En celdas, y acotado a que a cada lado le quede una: el reparto
            // ya sabe colapsar lo que no cabe, pero un cero escrito en el
            // árbol se queda escrito.
            let celdas = f32::from(celdas_del_par);
            // El redondeo y el corte, en UN sitio: `f32` a `u16` trunca y no
            // tiene signo, así que el clamp va antes de convertir y no
            // después — un `as` sobre un negativo o sobre 70000 no avisa.
            let entero = |v: f32| -> u16 {
                let v = v.round().clamp(1.0, f32::from(u16::MAX));
                // Ya está entre 1 y `u16::MAX` y sin parte fraccionaria: la
                // conversión no puede perder nada.
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "el clamp de la línea de arriba deja el valor dentro de u16 y entero"
                )]
                let v = v as u16;
                v
            };
            let izq_celdas = (celdas * frac).round().clamp(1.0, (celdas - 1.0).max(1.0));
            match (ns[pos], siguiente) {
                (Size::Fixed(_), Size::Fixed(_)) => {
                    ns[pos] = Size::Fixed(entero(izq_celdas));
                    ns[pos + 1] = Size::Fixed(entero(celdas - izq_celdas));
                }
                // Un fijo contra un ponderado: se escribe el FIJO y el otro se
                // queda con lo que sobre, que es lo que el reparto ya hacía.
                // Escribir los dos convertiría un ponderado en fijo por
                // arrastrar su borde, y con eso dejaría de estirarse al
                // cambiar el tamaño de la ventana.
                (Size::Fixed(_), _) => ns[pos] = Size::Fixed(entero(izq_celdas)),
                (_, Size::Fixed(_)) => {
                    ns[pos + 1] = Size::Fixed(entero(celdas - izq_celdas));
                }
                (Size::Weight(_), Size::Weight(_)) => {
                    let izq = (f32::from(PESO_FINO) * frac).round().max(1.0);
                    ns[pos] = Size::Weight(entero(izq));
                    ns[pos + 1] = Size::Weight(entero(f32::from(PESO_FINO) - izq));
                }
                _ => {}
            }
            ns
        })
    }

    /// Deja a todos los hermanos ponderados del hueco `id` con el mismo peso.
    #[must_use]
    pub fn equalize(&self, id: SlotId) -> Self {
        self.map_split_of(id, &|sizes, _| {
            sizes
                .iter()
                .map(|s| match s {
                    Size::Weight(_) => Size::Weight(1),
                    otro => *otro,
                })
                .collect()
        })
    }

    /// El tamaño con el que reparte el `Split` que contiene `id`, y en qué
    /// posición está su hijo.
    ///
    /// Es lo que [`Self::resize`] cambia, para poder MIRARLO: sin esto, un
    /// test del ancho de un panel acaba comparando árboles enteros o llamando
    /// a `resize` con un id que el llamante de verdad no produce — que es
    /// exactamente cómo #244 M1 pasó desapercibida.
    ///
    /// ```
    /// use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};
    ///
    /// let arbol = Node::split(
    ///     Dir::Horizontal,
    ///     vec![
    ///         Node::slot(SlotId(1), KindId::browser()),
    ///         Node::slot(SlotId(2), KindId::browser()),
    ///     ],
    /// );
    /// let (sizes, pos) = arbol.sizes_of(SlotId(2)).expect("está en un split");
    /// assert_eq!((sizes[pos], pos), (Size::Weight(1), 1));
    /// assert!(Node::slot(SlotId(1), KindId::browser()).sizes_of(SlotId(1)).is_none());
    /// ```
    #[must_use]
    pub fn sizes_of(&self, id: SlotId) -> Option<(&[Size], usize)> {
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        for c in hijos {
            if c.contains(id)
                && !matches!(c, Self::Slot { .. })
                && let Some(dentro) = c.sizes_of(id)
            {
                return Some(dentro);
            }
        }
        if let Self::Split {
            children, sizes, ..
        } = self
            && let Some(pos) = children.iter().position(|c| c.contains(id))
        {
            return Some((sizes.as_slice(), pos));
        }
        None
    }

    /// Aplica `f` a los tamaños del `Split` que contiene `id`, dándole la
    /// posición del hijo que lo contiene.
    fn map_split_of(&self, id: SlotId, f: &dyn Fn(&[Size], usize) -> Vec<Size>) -> Self {
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return self.clone(),
        };
        for (i, c) in hijos.iter().enumerate() {
            if c.contains(id) && !matches!(c, Self::Slot { .. }) {
                let dentro = c.map_split_of(id, f);
                if dentro != *c {
                    return self.with_child(i, dentro);
                }
            }
        }
        if let Self::Split {
            dir,
            children,
            sizes,
        } = self
            && let Some(pos) = children.iter().position(|c| c.contains(id))
        {
            return Self::Split {
                dir: *dir,
                children: children.clone(),
                sizes: f(sizes, pos),
            };
        }
        self.clone()
    }

    /// Los huecos que se VERÍAN: como [`Self::slot_ids`], pero de cada
    /// [`Node::Tabs`] solo la pestaña activa.
    ///
    /// No es lo mismo que las colocaciones de un reparto —esto no sabe si algo
    /// cabe— y por eso existe: hay que saber quién queda visible justo DESPUÉS
    /// de tocar el árbol, antes de que haya un frame que repartir.
    #[must_use]
    pub fn visible_slot_ids(&self) -> Vec<SlotId> {
        let mut out = Vec::new();
        self.collect_visible(&mut out);
        out
    }

    fn collect_visible(&self, out: &mut Vec<SlotId>) {
        match self {
            Self::Split { children, .. } => {
                for c in children {
                    c.collect_visible(out);
                }
            }
            Self::Tabs { children, active } => {
                if let Some(c) = children.get(*active).or_else(|| children.first()) {
                    c.collect_visible(out);
                }
            }
            Self::Slot { id, .. } => out.push(*id),
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

    /// ¿Está el hueco `id` en este subárbol?
    #[must_use]
    pub fn contains(&self, id: SlotId) -> bool {
        self.slot_ids().contains(&id)
    }

    /// El mismo árbol con el hueco `id` metido en una [`Node::Tabs`] de una
    /// sola pestaña. Si ya es hijo directo de una `Tabs`, no cambia nada.
    #[must_use]
    pub fn wrap_in_tabs(&self, id: SlotId) -> Self {
        match self {
            Self::Tabs { children, active } => {
                if children
                    .iter()
                    .any(|c| matches!(c, Self::Slot { id: i, .. } if *i == id))
                {
                    return self.clone();
                }
                Self::Tabs {
                    children: children.iter().map(|c| c.wrap_in_tabs(id)).collect(),
                    active: *active,
                }
            }
            Self::Split {
                dir,
                children,
                sizes,
            } => Self::Split {
                dir: *dir,
                sizes: sizes.clone(),
                children: children.iter().map(|c| c.wrap_in_tabs(id)).collect(),
            },
            Self::Slot { id: i, .. } if *i == id => Self::Tabs {
                children: vec![self.clone()],
                active: 0,
            },
            Self::Slot { .. } => self.clone(),
        }
    }

    /// Abre `nuevo` como pestaña al lado de `id`, y la deja activa.
    ///
    /// Si `id` no estaba en pestañas, lo envuelve primero: abrir una pestaña
    /// desde un panel suelto es lo que convierte ese panel en el primero de un
    /// grupo, y pedirle al usuario dos pasos para eso no tendría sentido.
    #[must_use]
    pub fn add_tab(&self, id: SlotId, nuevo: &Self) -> Self {
        let envuelto = self.wrap_in_tabs(id);
        envuelto.insert_tab_near(id, nuevo).unwrap_or(envuelto)
    }

    fn insert_tab_near(&self, id: SlotId, nuevo: &Self) -> Option<Self> {
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        // Más adentro primero: la `Tabs` que manda es la INTERIOR, no la que
        // envuelve media pantalla.
        for (i, c) in hijos.iter().enumerate() {
            if let Some(cambiado) = c.insert_tab_near(id, nuevo) {
                return Some(self.with_child(i, cambiado));
            }
        }
        if let Self::Tabs { children, .. } = self
            && let Some(pos) = children.iter().position(|c| c.contains(id))
        {
            let mut nuevos = children.clone();
            nuevos.insert(pos + 1, nuevo.clone());
            return Some(Self::Tabs {
                children: nuevos,
                active: pos + 1,
            });
        }
        None
    }

    /// Cierra la pestaña que contiene `id`.
    ///
    /// Una `Tabs` que se queda con UN hijo se DISUELVE en él: un grupo de una
    /// pestaña no es un grupo, y dejarlo pintaría una barra de pestañas con una
    /// sola entrada para siempre.
    ///
    /// Devuelve `None` si `id` no está dentro de ninguna `Tabs` — cerrar un
    /// panel suelto es `layout.close-slot`, no `pane.tab-close`.
    #[must_use]
    pub fn close_tab(&self, id: SlotId) -> Option<Self> {
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        for (i, c) in hijos.iter().enumerate() {
            if let Some(cambiado) = c.close_tab(id) {
                return Some(self.with_child(i, cambiado));
            }
        }
        if let Self::Tabs { children, active } = self
            && children.len() > 1
            && let Some(pos) = children.iter().position(|c| c.contains(id))
        {
            let mut nuevos = children.clone();
            nuevos.remove(pos);
            if nuevos.len() == 1 {
                return nuevos.into_iter().next();
            }
            // Cerrar una pestaña ANTERIOR a la activa arrastra el índice; si
            // no, el activo pasaría a nombrar a la de al lado. El clamp va
            // DESPUÉS del arrastre: al revés se comen los dos y el activo cae
            // una posición de más.
            let act = if pos < *active {
                active.saturating_sub(1)
            } else {
                *active
            }
            .min(nuevos.len() - 1);
            return Some(Self::Tabs {
                children: nuevos,
                active: act,
            });
        }
        None
    }

    /// Las pestañas del grupo que contiene `id`: el hueco que encabeza cada
    /// una y cuál está activa. `None` si `id` no está en un grupo.
    ///
    /// Lo usa el render de la barra de pestañas, y por eso devuelve el PRIMER
    /// hueco de cada pestaña: es de quien se saca el título.
    #[must_use]
    pub fn tabs_of(&self, id: SlotId) -> Option<(Vec<SlotId>, usize)> {
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return None,
        };
        for c in hijos {
            if let Some(v) = c.tabs_of(id) {
                return Some(v);
            }
        }
        if let Self::Tabs { children, active } = self
            && children.iter().any(|c| c.contains(id))
        {
            return Some((
                children.iter().filter_map(Self::first_slot_id).collect(),
                (*active).min(children.len().saturating_sub(1)),
            ));
        }
        None
    }

    /// El mismo árbol con el hueco `id` VISIBLE: activa su pestaña en cada
    /// grupo del camino (#329).
    ///
    /// Existe porque [`Self::slot_ids`] y [`Self::visible_slot_ids`] contestan
    /// dos preguntas distintas —«¿existe?» y «¿se ve?»— y hay una tercera que
    /// no tenía respuesta: «que se vea». Sin ella, quien encontraba un hueco
    /// escondido solo podía mandarle el teclado, que es enfocar algo que el
    /// lector no tiene delante.
    ///
    /// Recorre TODO el camino y no solo el grupo de dentro: activar la pestaña
    /// interior dejando la exterior en otra deja el hueco igual de invisible, y
    /// el llamante creería haberlo enseñado. Un hueco que no está devuelve el
    /// árbol igual — esto asegura un invariante, no ejecuta un gesto.
    ///
    /// Uno que ya se ve lo devuelve igual salvo en un caso, y conviene decirlo:
    /// el tipo permite un `active` fuera de rango, que [`Self::visible_slot_ids`]
    /// y el reparto clampan los dos al primero. Sobre uno así, revelar el hueco
    /// que YA se veía escribe el índice de verdad. Normaliza, no mueve nada de
    /// sitio.
    ///
    /// ```
    /// use norte_frontend::layout::{KindId, Node, SlotId};
    /// let arbol = Node::Tabs {
    ///     children: vec![
    ///         Node::slot(SlotId(1), KindId::browser()),
    ///         Node::slot(SlotId(2), KindId::new("log")),
    ///     ],
    ///     active: 0,
    /// };
    /// assert!(!arbol.visible_slot_ids().contains(&SlotId(2)));
    /// assert!(arbol.reveal(SlotId(2)).visible_slot_ids().contains(&SlotId(2)));
    /// ```
    #[must_use]
    pub fn reveal(&self, id: SlotId) -> Self {
        match self {
            Self::Slot { .. } => self.clone(),
            Self::Split {
                dir,
                children,
                sizes,
            } => Self::Split {
                dir: *dir,
                children: children
                    .iter()
                    .map(|c| {
                        if c.contains(id) {
                            c.reveal(id)
                        } else {
                            c.clone()
                        }
                    })
                    .collect(),
                sizes: sizes.clone(),
            },
            // El `active` de entrada no se lee a propósito: revelar no lo
            // conserva ni lo mueve un paso, lo FIJA en la pestaña que contiene
            // al hueco. Ese es todo el gesto.
            Self::Tabs { children, .. } => {
                let Some(pos) = children.iter().position(|c| c.contains(id)) else {
                    return self.clone();
                };
                Self::Tabs {
                    children: children
                        .iter()
                        .enumerate()
                        .map(|(i, c)| if i == pos { c.reveal(id) } else { c.clone() })
                        .collect(),
                    active: pos,
                }
            }
        }
    }

    /// Deja activa la pestaña `i` del grupo que contiene `id`.
    #[must_use]
    pub fn set_active_for(&self, id: SlotId, i: usize) -> Self {
        self.map_tabs_of(id, &|children, _| {
            (children.to_vec(), i.min(children.len() - 1))
        })
    }

    /// Mueve la pestaña que contiene `id` `delta` posiciones, sin salirse.
    #[must_use]
    pub fn move_tab(&self, id: SlotId, delta: isize) -> Self {
        self.map_tabs_of(id, &|children, pos| {
            let destino = pos
                .saturating_add_signed(delta)
                .min(children.len().saturating_sub(1));
            let mut nuevos = children.to_vec();
            let quien = nuevos.remove(pos);
            nuevos.insert(destino, quien);
            (nuevos, destino)
        })
    }

    /// Aplica `f` a la `Tabs` que contiene `id`, dándole sus hijos y la
    /// posición del que lo contiene, y esperando los hijos nuevos y el activo.
    fn map_tabs_of(&self, id: SlotId, f: &ReTab<'_>) -> Self {
        let hijos = match self {
            Self::Split { children, .. } | Self::Tabs { children, .. } => children,
            Self::Slot { .. } => return self.clone(),
        };
        for (i, c) in hijos.iter().enumerate() {
            if c.contains(id) && !matches!(c, Self::Slot { .. }) {
                let dentro = c.map_tabs_of(id, f);
                if dentro != *c {
                    return self.with_child(i, dentro);
                }
            }
        }
        if let Self::Tabs { children, .. } = self
            && let Some(pos) = children.iter().position(|c| c.contains(id))
        {
            let (nuevos, act) = f(children, pos);
            return Self::Tabs {
                children: nuevos,
                active: act,
            };
        }
        self.clone()
    }

    /// El mismo nodo con el hijo `i` sustituido.
    fn with_child(&self, i: usize, hijo: Self) -> Self {
        match self {
            Self::Split {
                dir,
                children,
                sizes,
            } => {
                let mut nuevos = children.clone();
                if let Some(slot) = nuevos.get_mut(i) {
                    *slot = hijo;
                }
                Self::Split {
                    dir: *dir,
                    children: nuevos,
                    sizes: sizes.clone(),
                }
            }
            Self::Tabs { children, active } => {
                let mut nuevos = children.clone();
                if let Some(slot) = nuevos.get_mut(i) {
                    *slot = hijo;
                }
                Self::Tabs {
                    children: nuevos,
                    active: *active,
                }
            }
            Self::Slot { .. } => self.clone(),
        }
    }

    /// Una copia cuyos huecos se numeran desde `base`, más el mapa
    /// viejo → nuevo.
    ///
    /// Es lo que hace que dos perfiles no se pisen (spec 2026-08-26, D5): las
    /// disposiciones de fábrica usan 1..=8 TODAS, así que adoptar la misma en
    /// dos perfiles sin reasignar deja los dos compartiendo el hueco 1 —
    /// mismo directorio, mismo historial, mismas marcas.
    ///
    /// El mapa NO es una comodidad. `[profile.start]` viene indexado por los
    /// ids que el fichero de disposición del perfil escribe, así que aplicarlo
    /// después de rebasar exige la traducción; devolver solo el árbol dejaría
    /// esas claves inservibles.
    ///
    /// El orden de asignación es el de [`Self::slot_ids`], que es el de
    /// lectura: determinista, y por tanto el mismo árbol rebasado dos veces
    /// desde la misma base da el mismo resultado.
    ///
    /// # De dónde sale `base`
    ///
    /// De [`crate::session::SessionBody::next_slot_base`], y de ningún otro
    /// sitio. La propiedad de «no colisiona» vive ENTERA ahí: mirar solo el
    /// árbol del perfil activo daría una base que aterriza encima de los
    /// huecos huérfanos, que son justo los que nadie está mirando cuando pasa.
    /// Y el árbol rebasado se mete en `layouts` ANTES de volver a pedir una
    /// base, o dos perfiles rebasan desde el mismo número.
    ///
    /// Sin espacio libre por arriba devuelve el árbol SIN TOCAR y un mapa
    /// vacío: el llamante se queda como estaba en vez de recibir un árbol con
    /// ids repetidos.
    ///
    /// ```
    /// use norte_frontend::layout::{Dir, KindId, Node, SlotId};
    ///
    /// let arbol = Node::split(
    ///     Dir::Horizontal,
    ///     vec![
    ///         Node::slot(SlotId(1), KindId::browser()),
    ///         Node::slot(SlotId(2), KindId::browser()),
    ///     ],
    /// );
    /// let (nuevo, mapa) = arbol.rebase_slot_ids(100);
    /// assert_eq!(nuevo.slot_ids(), vec![SlotId(100), SlotId(101)]);
    /// assert_eq!(mapa[&SlotId(2)], SlotId(101));
    /// ```
    #[must_use]
    pub fn rebase_slot_ids(&self, base: u32) -> (Self, std::collections::BTreeMap<SlotId, SlotId>) {
        let mut mapa = std::collections::BTreeMap::new();
        let mut siguiente = base;
        for id in self.slot_ids() {
            // Un árbol con ids repetidos es incoherente de entrada
            // (`duplicate_slot_ids` lo dice y `validate` lo rechaza); si llega
            // uno, los dos huecos siguen compartiendo id en vez de que uno se
            // lleve un número que nadie le dio.
            if mapa.contains_key(&id) {
                continue;
            }
            // Sin espacio arriba se DEVUELVE EL ÁRBOL TAL CUAL, y esto no es
            // celo: saturar era peor que envolver. `saturating_add` deja a
            // todos los huecos siguientes con `u32::MAX`, así que un árbol de
            // entrada sano salía con ids REPETIDOS; ese árbol se guarda en
            // `layouts`, y el siguiente `from_value` lo valida y devuelve
            // `BadLayout` para el cuerpo ENTERO — el estado de todos los
            // perfiles, no el del roto. Ésa es la pérdida que ADR 0059 promete
            // que no pasa.
            let Some(tope) = siguiente.checked_add(1) else {
                return (self.clone(), std::collections::BTreeMap::new());
            };
            mapa.insert(id, SlotId(siguiente));
            siguiente = tope;
        }
        (self.remap_slot_ids(&mapa), mapa)
    }

    /// Aplica un mapa de ids a una copia del árbol. Lo que no esté en el mapa
    /// se queda como está.
    fn remap_slot_ids(&self, mapa: &std::collections::BTreeMap<SlotId, SlotId>) -> Self {
        match self {
            Self::Split {
                dir,
                children,
                sizes,
            } => Self::Split {
                dir: *dir,
                children: children.iter().map(|c| c.remap_slot_ids(mapa)).collect(),
                sizes: sizes.clone(),
            },
            Self::Tabs { children, active } => Self::Tabs {
                children: children.iter().map(|c| c.remap_slot_ids(mapa)).collect(),
                active: *active,
            },
            Self::Slot {
                id,
                kind,
                params,
                bindings,
            } => Self::Slot {
                id: mapa.get(id).copied().unwrap_or(*id),
                kind: kind.clone(),
                params: params.clone(),
                bindings: *bindings,
            },
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

    /// **Sacar a la luz un hueco escondido activa SU pestaña** (#329).
    ///
    /// La barra de paneles y los toggles preguntan «¿existe?» y actúan como si
    /// hubieran preguntado «¿se ve?». Esto es la mitad que faltaba: poder
    /// contestar «que se vea».
    #[test]
    fn revelar_activa_la_pestana_del_hueco() {
        let arbol = Node::Tabs {
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::new("log")),
            ],
            active: 0,
        };
        assert!(!arbol.visible_slot_ids().contains(&SlotId(2)));
        let visto = arbol.reveal(SlotId(2));
        assert!(visto.visible_slot_ids().contains(&SlotId(2)));
        assert!(
            !visto.visible_slot_ids().contains(&SlotId(1)),
            "activar una pestaña esconde a su hermana: es lo que significa"
        );
    }

    /// Y lo hace en CADA grupo del camino, no solo en el de dentro.
    ///
    /// Con grupos anidados, activar el interior y dejar el exterior en otra
    /// pestaña deja el hueco tan invisible como estaba, y el llamante creería
    /// haberlo enseñado.
    #[test]
    fn revelar_atraviesa_los_grupos_anidados() {
        let dentro = Node::Tabs {
            children: vec![
                Node::slot(SlotId(3), KindId::browser()),
                Node::slot(SlotId(4), KindId::new("log")),
            ],
            active: 0,
        };
        let arbol = Node::Tabs {
            children: vec![Node::slot(SlotId(5), KindId::browser()), dentro],
            active: 0,
        };
        assert!(!arbol.visible_slot_ids().contains(&SlotId(4)));
        let visto = arbol.reveal(SlotId(4));
        assert!(
            visto.visible_slot_ids().contains(&SlotId(4)),
            "el grupo de fuera seguía enseñando la otra pestaña"
        );
    }

    /// Con un `Split` por el camino, revelar respeta TODO lo demás: los
    /// tamaños, y la pestaña activa de un grupo que no contiene al hueco.
    ///
    /// El riesgo de una función que reconstruye el árbol es perder por el
    /// camino algo que nadie mira en el test, y aquí lo que se perdería son
    /// medidas: un `Split` que vuelve con pesos por defecto reparte la pantalla
    /// de otra manera sin que nada se ponga rojo.
    #[test]
    fn revelar_conserva_medidas_y_los_grupos_ajenos() {
        let ajeno = Node::Tabs {
            children: vec![
                Node::slot(SlotId(10), KindId::browser()),
                Node::slot(SlotId(11), KindId::browser()),
            ],
            active: 1,
        };
        let arbol = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Fixed(24), Size::Weight(1)],
            children: vec![
                ajeno,
                Node::Tabs {
                    children: vec![
                        Node::slot(SlotId(1), KindId::browser()),
                        Node::slot(SlotId(2), KindId::new("log")),
                    ],
                    active: 0,
                },
            ],
        };
        let visto = arbol.reveal(SlotId(2));
        assert!(visto.visible_slot_ids().contains(&SlotId(2)));
        assert!(
            visto.visible_slot_ids().contains(&SlotId(11)),
            "el grupo de al lado no se toca: no contiene al hueco"
        );
        let (sizes, _) = visto.sizes_of(SlotId(2)).expect("sigue en el split");
        assert_eq!(
            (sizes[0], sizes[1]),
            (Size::Fixed(24), Size::Weight(1)),
            "reconstruir el split se llevó por delante las medidas"
        );
    }

    /// Un hueco que ya se ve —o que no está— no mueve nada: revelar no es un
    /// gesto, es un invariante que se asegura.
    #[test]
    fn revelar_lo_que_ya_se_ve_no_cambia_el_arbol() {
        let arbol = Node::Tabs {
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::new("log")),
            ],
            active: 1,
        };
        assert_eq!(arbol.reveal(SlotId(2)), arbol);
        assert_eq!(arbol.reveal(SlotId(99)), arbol, "y uno que no está tampoco");
    }

    /// Arrastrar el borde pone el hueco donde dice el puntero, y lo que uno
    /// gana lo pierde su vecino.
    ///
    /// Con pesos se renormaliza la pareja: dos huecos por defecto son
    /// `Weight(1)` y `Weight(1)`, y sobre esa pareja el único borde posible
    /// sería la mitad exacta — un arrastre que solo puede aterrizar en el
    /// centro no es un arrastre.
    #[test]
    fn arrastrar_el_borde_reparte_la_pareja() {
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let movido = arbol.drag_border(SlotId(1), 0.25, 80);
        let (sizes, pos) = movido.sizes_of(SlotId(1)).expect("está en un split");
        assert_eq!(pos, 0);
        assert_eq!(
            (sizes[0], sizes[1]),
            (Size::Weight(25), Size::Weight(75)),
            "un cuarto para el de la izquierda, y el resto para el otro"
        );
    }

    /// Ni el uno ni el otro pueden desaparecer: un hueco a cero se lleva con
    /// él la forma de devolverlo.
    #[test]
    fn arrastrar_hasta_el_extremo_deja_hueco_a_los_dos() {
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        for frac in [-3.0, 0.0, 1.0, 4.0] {
            let movido = arbol.drag_border(SlotId(1), frac, 80);
            let (sizes, _) = movido.sizes_of(SlotId(1)).expect("split");
            for s in &sizes[..2] {
                assert!(
                    matches!(s, Size::Weight(w) if *w >= 1),
                    "con frac={frac} alguien se quedó sin sitio: {sizes:?}"
                );
            }
        }
    }

    /// Un FIJO se escribe en celdas —es lo que significa— y su vecino
    /// ponderado no se convierte en fijo: si lo hiciera, dejaría de estirarse
    /// al cambiar el tamaño de la ventana.
    #[test]
    fn arrastrar_el_borde_de_un_fijo_lo_escribe_en_celdas() {
        let arbol = Node::Split {
            dir: Dir::Horizontal,
            children: vec![
                Node::slot(SlotId(1), KindId::new("places")),
                Node::slot(SlotId(2), KindId::browser()),
            ],
            sizes: vec![Size::Fixed(16), Size::Weight(1)],
        };
        let movido = arbol.drag_border(SlotId(1), 0.5, 100);
        let (sizes, _) = movido.sizes_of(SlotId(1)).expect("split");
        assert_eq!(sizes[0], Size::Fixed(50), "la mitad de cien celdas");
        assert_eq!(sizes[1], Size::Weight(1), "el ponderado sigue ponderado");
    }

    /// Las disposiciones de fábrica usan 1..=8, TODAS. Sin rebase, dos perfiles
    /// comparten el hueco 1 y se pisan el directorio y el historial — que es
    /// exactamente el bug que los perfiles existen para arreglar.
    #[test]
    fn rebase_reasigna_desde_la_base_y_devuelve_el_mapa() {
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let (nuevo, mapa) = arbol.rebase_slot_ids(100);
        assert_eq!(nuevo.slot_ids(), vec![SlotId(100), SlotId(101)]);
        assert_eq!(mapa.get(&SlotId(1)), Some(&SlotId(100)));
        assert_eq!(mapa.get(&SlotId(2)), Some(&SlotId(101)));
    }

    /// Rebasar no puede cambiar la FORMA: mismo árbol, mismos kinds, mismos
    /// tamaños. Solo los números.
    #[test]
    fn rebase_conserva_la_forma() {
        let arbol = Node::split(
            Dir::Vertical,
            vec![
                Node::slot(SlotId(3), KindId::new("places")),
                Node::split(
                    Dir::Horizontal,
                    vec![
                        Node::slot(SlotId(1), KindId::browser()),
                        Node::slot(SlotId(2), KindId::new("viewer")),
                    ],
                ),
            ],
        );
        let (nuevo, _) = arbol.rebase_slot_ids(50);
        assert_eq!(nuevo.slot_ids().len(), arbol.slot_ids().len());
        assert!(crate::layout::validate(&nuevo).is_ok());
    }

    /// Un árbol sano rebasado no puede FABRICAR un duplicado, sea cual sea el
    /// orden de los ids originales.
    #[test]
    fn rebase_no_fabrica_duplicados() {
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(7), KindId::browser()),
                Node::slot(SlotId(1), KindId::browser()),
            ],
        );
        let (nuevo, _) = arbol.rebase_slot_ids(10);
        assert!(nuevo.duplicate_slot_ids().is_empty());
    }

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

    fn b(id: u32) -> Node {
        Node::slot(SlotId(id), KindId::browser())
    }

    /// Abrir una pestaña desde un panel suelto lo envuelve y deja activa la
    /// nueva. Pedir dos pasos para eso no tendría sentido.
    #[test]
    fn abrir_una_pestana_desde_un_panel_suelto_lo_envuelve() {
        let arbol = Node::split(Dir::Horizontal, vec![b(1), b(2)]);
        let nuevo = arbol.add_tab(SlotId(2), &b(9));
        assert_eq!(nuevo.slot_ids(), vec![SlotId(1), SlotId(2), SlotId(9)]);
        assert_eq!(
            nuevo.tabs_of(SlotId(2)),
            Some((vec![SlotId(2), SlotId(9)], 1))
        );
    }

    /// La `Tabs` que manda es la INTERIOR, no la que envuelve media pantalla.
    #[test]
    fn una_pestana_nueva_entra_en_el_grupo_mas_interior() {
        let arbol = Node::Tabs {
            children: vec![Node::split(
                Dir::Horizontal,
                vec![
                    b(1),
                    Node::Tabs {
                        children: vec![b(2)],
                        active: 0,
                    },
                ],
            )],
            active: 0,
        };
        let nuevo = arbol.add_tab(SlotId(2), &b(9));
        assert_eq!(
            nuevo.tabs_of(SlotId(2)),
            Some((vec![SlotId(2), SlotId(9)], 1))
        );
        // El grupo de fuera sigue con una sola pestaña.
        assert_eq!(nuevo.tabs_of(SlotId(1)), Some((vec![SlotId(1)], 0)));
    }

    /// Una `Tabs` que se queda con UN hijo se DISUELVE: un grupo de una
    /// pestaña no es un grupo, y dejarlo pintaría una barra con una sola
    /// entrada para siempre.
    #[test]
    fn cerrar_la_penultima_pestana_disuelve_el_grupo() {
        let arbol = Node::Tabs {
            children: vec![b(1), b(2)],
            active: 1,
        };
        let nuevo = arbol.close_tab(SlotId(2)).expect("estaba en un grupo");
        assert_eq!(nuevo, b(1), "el grupo desaparece y queda el hueco");
    }

    /// Cerrar un panel SUELTO no es `pane.tab-close`: devuelve `None` y el llamante
    /// decide (será `layout.close-slot`).
    #[test]
    fn cerrar_un_panel_suelto_no_es_cerrar_una_pestana() {
        let arbol = Node::split(Dir::Horizontal, vec![b(1), b(2)]);
        assert_eq!(arbol.close_tab(SlotId(2)), None);
    }

    /// Cerrar una pestaña anterior a la activa arrastra el índice activo: si
    /// no, el activo pasaría a nombrar a la pestaña de al lado.
    #[test]
    fn cerrar_una_pestana_anterior_arrastra_el_activo() {
        let arbol = Node::Tabs {
            children: vec![b(1), b(2), b(3)],
            active: 2,
        };
        let nuevo = arbol.close_tab(SlotId(1)).expect("está en el grupo");
        assert_eq!(
            nuevo.tabs_of(SlotId(3)),
            Some((vec![SlotId(2), SlotId(3)], 1))
        );
    }

    /// Mover una pestaña se la lleva el activo con ella.
    #[test]
    fn mover_una_pestana_se_lleva_el_activo() {
        let arbol = Node::Tabs {
            children: vec![b(1), b(2), b(3)],
            active: 0,
        };
        let nuevo = arbol.move_tab(SlotId(1), 2);
        assert_eq!(
            nuevo.tabs_of(SlotId(1)),
            Some((vec![SlotId(2), SlotId(3), SlotId(1)], 2))
        );
    }

    /// Mover más allá del borde se queda en el borde, no da la vuelta: una
    /// pestaña que salta de la última a la primera al pulsar una vez de más es
    /// exactamente lo que nadie quería.
    #[test]
    fn mover_una_pestana_no_da_la_vuelta() {
        let arbol = Node::Tabs {
            children: vec![b(1), b(2)],
            active: 1,
        };
        let nuevo = arbol.move_tab(SlotId(2), 5);
        assert_eq!(
            nuevo.tabs_of(SlotId(2)),
            Some((vec![SlotId(1), SlotId(2)], 1))
        );
    }

    /// Partir un hueco lo deja con el nuevo al lado, los dos al mismo peso.
    #[test]
    fn partir_un_hueco_deja_a_los_dos_al_mismo_peso() {
        let arbol = b(1);
        let nuevo = arbol.split_slot(SlotId(1), Dir::Vertical, &b(9));
        assert_eq!(nuevo.slot_ids(), vec![SlotId(1), SlotId(9)]);
        let Node::Split { dir, sizes, .. } = &nuevo else {
            panic!("split")
        };
        assert_eq!(*dir, Dir::Vertical);
        assert_eq!(*sizes, vec![Size::Weight(1), Size::Weight(1)]);
    }

    /// Partir OTRA VEZ en el mismo eje da TERCIOS, no un cuarto.
    ///
    /// El corte se une al `Split` que ya corre en ese eje en vez de envolver
    /// el hueco en uno nuevo. Anidando, cada partición se llevaba la mitad de
    /// la mitad: tres paneles quedaban en 1/2, 1/4 y 1/4, y a la cuarta el
    /// hijo más profundo bajaba del mínimo del kind y el reparto lo degradaba
    /// a pestañas — el panel recién pedido desaparecía sin decir nada.
    #[test]
    fn partir_en_el_mismo_eje_reparte_a_partes_iguales() {
        let arbol = b(1).split_slot(SlotId(1), Dir::Vertical, &b(2));
        let tres = arbol.split_slot(SlotId(2), Dir::Vertical, &b(3));
        let Node::Split {
            children, sizes, ..
        } = &tres
        else {
            panic!("split")
        };
        assert_eq!(children.len(), 3, "un solo Split con tres hijos");
        assert_eq!(
            *sizes,
            vec![Size::Weight(1), Size::Weight(1), Size::Weight(1)]
        );
        assert_eq!(
            tres.slot_ids(),
            vec![SlotId(1), SlotId(2), SlotId(3)],
            "y el nuevo entra JUNTO al que se partió, no al final"
        );
    }

    /// En el OTRO eje sigue envolviendo: un corte perpendicular no puede
    /// entrar en la fila de sus hermanos.
    #[test]
    fn partir_en_el_otro_eje_sigue_anidando() {
        let arbol = b(1).split_slot(SlotId(1), Dir::Vertical, &b(2));
        let cruz = arbol.split_slot(SlotId(2), Dir::Horizontal, &b(3));
        let Node::Split { children, dir, .. } = &cruz else {
            panic!("split")
        };
        assert_eq!(*dir, Dir::Vertical);
        assert_eq!(children.len(), 2, "el de fuera sigue teniendo dos hijos");
        assert!(
            matches!(&children[1], Node::Split { dir, .. } if *dir == Dir::Horizontal),
            "y el corte nuevo va DENTRO del que se partió"
        );
    }

    /// Un hueco de tamaño FIJO se parte por dentro, no se une a sus hermanos:
    /// su tamaño es cromo acoplado, y meter otro hijo en esa fila le robaría
    /// el sitio a lo que hay al lado.
    #[test]
    fn partir_un_hueco_fijo_no_se_une_a_sus_hermanos() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(8)],
            children: vec![b(1), b(2)],
        };
        let partido = arbol.split_slot(SlotId(2), Dir::Vertical, &b(3));
        let Node::Split {
            children, sizes, ..
        } = &partido
        else {
            panic!("split")
        };
        assert_eq!(children.len(), 2, "sigue habiendo dos hijos arriba");
        assert_eq!(sizes[1], Size::Fixed(8), "y el fijo conserva su tamaño");
        assert_eq!(children[1].slot_ids(), vec![SlotId(2), SlotId(3)]);
    }

    /// Partir una PESTAÑA parte lo que estás mirando, no reorganiza sus
    /// hermanas: el corte va dentro de la pestaña, no alrededor del grupo.
    #[test]
    fn partir_una_pestana_corta_dentro_de_ella() {
        let arbol = Node::Tabs {
            children: vec![b(1), b(2)],
            active: 1,
        };
        let nuevo = arbol.split_slot(SlotId(2), Dir::Horizontal, &b(9));
        let Node::Tabs { children, active } = &nuevo else {
            panic!("sigue siendo un grupo de pestañas")
        };
        assert_eq!(*active, 1, "la pestaña activa no se mueve");
        assert_eq!(children.len(), 2, "sigue habiendo DOS pestañas");
        assert_eq!(children[1].slot_ids(), vec![SlotId(2), SlotId(9)]);
    }

    #[test]
    fn el_arbol_round_trippea_en_toml() {
        use crate::layout::{Dir, KindId, Node, Size, SlotId};
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Auto, Size::Fixed(1)],
            children: vec![
                Node::split(
                    Dir::Horizontal,
                    vec![
                        Node::slot(SlotId(1), KindId::browser()),
                        Node::slot(SlotId(2), KindId::browser()),
                    ],
                ),
                Node::slot(SlotId(3), KindId::new("tasks")),
                Node::slot(SlotId(4), KindId::new("status")),
            ],
        };
        let t = toml::to_string_pretty(&arbol).expect("serializa a TOML");
        println!("---\n{t}\n---");
        let vuelta: Node = toml::from_str(&t).expect("vuelve de TOML");
        assert_eq!(vuelta, arbol);
    }

    /// Cerrar un hueco deja al hermano ocupando el sitio de los dos.
    #[test]
    fn cerrar_un_hueco_disuelve_el_split_de_dos() {
        let arbol = Node::split(Dir::Horizontal, vec![b(1), b(2)]);
        assert_eq!(arbol.close_slot(SlotId(2)), Some(b(1)));
    }

    /// Cerrar el ÚNICO hueco devuelve `None`: una pantalla sin nada no la
    /// decide el árbol.
    #[test]
    fn cerrar_el_unico_hueco_no_se_hace_solo() {
        assert_eq!(b(1).close_slot(SlotId(1)), None);
    }

    /// Al cerrar, el tamaño del hueco se va CON él: dejarlo desplazaría todos
    /// los pesos una posición y el reparto pasaría a ser otro sin avisar.
    #[test]
    fn cerrar_un_hueco_se_lleva_su_tamano() {
        let arbol = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Weight(3), Size::Weight(1), Size::Weight(1)],
            children: vec![b(1), b(2), b(3)],
        };
        let nuevo = arbol.close_slot(SlotId(1)).expect("quedan dos");
        let Node::Split { sizes, .. } = &nuevo else {
            panic!("sigue siendo un split")
        };
        assert_eq!(*sizes, vec![Size::Weight(1), Size::Weight(1)]);
    }

    /// Agrandar toca el peso del hueco enfocado, con tope.
    #[test]
    fn agrandar_sube_el_peso_hasta_el_tope() {
        let arbol = Node::split(Dir::Horizontal, vec![b(1), b(2)]);
        let mut a = arbol;
        for _ in 0..20 {
            a = a.resize(SlotId(1), 1);
        }
        let Node::Split { sizes, .. } = &a else {
            panic!("split")
        };
        assert_eq!(sizes[0], Size::Weight(10), "no crece sin fin");
    }

    /// Un hijo FIJO SÍ se agranda desde #227, en celdas: era lo que dejaba el
    /// sidebar atascado en el ancho con el que se abría.
    ///
    /// Lo que protege a la barra de estado —el otro hijo fijo que hay en la
    /// pantalla— no es esta función: es que `layout.grow` solo nombra al hueco
    /// CON EL FOCO, y el kind `status` no es enfocable. Un tope aquí por el
    /// tamaño del hijo sería adivinar cuál de los dos fijos es un sidebar.
    #[test]
    fn agrandar_mueve_un_hijo_fijo_en_celdas() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(1)],
            children: vec![b(1), Node::slot(SlotId(2), KindId::new("status"))],
        };
        let nuevo = arbol.resize(SlotId(2), 3);
        let Node::Split { sizes, .. } = &nuevo else {
            panic!("split")
        };
        assert_eq!(sizes[1], Size::Fixed(7));
    }

    /// Igualar devuelve los pesos a uno y deja los fijos en paz.
    #[test]
    fn igualar_solo_toca_los_ponderados() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(7), Size::Fixed(2), Size::Weight(3)],
            children: vec![b(1), b(2), b(3)],
        };
        let nuevo = arbol.equalize(SlotId(1));
        let Node::Split { sizes, .. } = &nuevo else {
            panic!("split")
        };
        assert_eq!(
            *sizes,
            vec![Size::Weight(1), Size::Fixed(2), Size::Weight(1)]
        );
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

    /// Un dock a la izquierda entra en el `Split` HORIZONTAL que ya existe, no
    /// alrededor del árbol entero: si envolviera la raíz, la barra de estado y
    /// la franja de tareas se quedarían a la DERECHA del sidebar en vez de
    /// debajo de los listados.
    #[test]
    fn dock_izquierda_entra_en_el_split_del_cuerpo() {
        let cuerpo = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let raiz = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(1)],
            children: vec![cuerpo, Node::slot(SlotId(4), KindId::new("status"))],
        };
        let con = raiz.dock(
            SlotId(1),
            Edge::Left,
            Size::Fixed(16),
            &Node::slot(SlotId(9), KindId::new("places")),
        );
        let Node::Split { children, .. } = &con else {
            panic!("la raíz sigue siendo un Split");
        };
        let Node::Split {
            children: cuerpo,
            sizes,
            dir,
        } = &children[0]
        else {
            panic!("el cuerpo sigue siendo un Split");
        };
        assert_eq!(*dir, Dir::Horizontal);
        assert_eq!(cuerpo.len(), 3);
        assert_eq!(cuerpo[0].first_slot_id(), Some(SlotId(9)));
        assert_eq!(sizes[0], Size::Fixed(16));
        // Y la barra de estado NO se movió: sigue siendo hija de la raíz.
        assert_eq!(children[1].first_slot_id(), Some(SlotId(4)));
    }

    /// A la derecha, al final del mismo split.
    #[test]
    fn dock_derecha_va_al_final() {
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let con = arbol.dock(
            SlotId(1),
            Edge::Right,
            Size::Weight(1),
            &Node::slot(SlotId(9), KindId::new("viewer")),
        );
        let Node::Split { children, .. } = &con else {
            panic!("split")
        };
        assert_eq!(children.len(), 3);
        assert_eq!(children[2].first_slot_id(), Some(SlotId(9)));
    }

    /// REGRESIÓN (captura del 2026-09-21): un PESO se mide contra los pesos
    /// hermanos, no en absoluto. Arrastrar el borde entre dos listados los
    /// deja en 49/51; un visor que entra con `Weight(1)` se quedaba con
    /// 1/101 del sitio libre — una barrita de un píxel que no se ve. Entra
    /// con la MEDIA de los pesos de sus hermanos, que es lo que `Weight(1)`
    /// significa en un reparto de unos.
    #[test]
    fn un_peso_que_entra_se_mide_contra_sus_hermanos() {
        let arbol = Node::Split {
            dir: Dir::Horizontal,
            children: vec![
                Node::slot(SlotId(5), KindId::new("places")),
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
            sizes: vec![Size::Fixed(16), Size::Weight(49), Size::Weight(51)],
        };
        let con = arbol.dock(
            SlotId(1),
            Edge::Right,
            Size::Weight(1),
            &Node::slot(SlotId(9), KindId::new("viewer")),
        );
        let Node::Split { sizes, .. } = &con else {
            panic!("split")
        };
        assert_eq!(sizes.last(), Some(&Size::Weight(50)), "{sizes:?}");
        // Un fijo no se toca: su número es de celdas, no de proporción.
        let con = arbol.dock(
            SlotId(1),
            Edge::Right,
            Size::Fixed(30),
            &Node::slot(SlotId(9), KindId::new("metadata")),
        );
        let Node::Split { sizes, .. } = &con else {
            panic!("split")
        };
        assert_eq!(sizes.last(), Some(&Size::Fixed(30)));
    }

    /// Un panel acoplado ABAJO entra por ENCIMA de la franja de tareas y de
    /// la barra de estado, no debajo (captura del 2026-09-21): el registro y
    /// procesos salían por debajo de la barra de estado. En VS Code el panel
    /// de abajo está siempre encima de la barra; y en el terminal, la barra
    /// de estado tiene que ser la última fila.
    #[test]
    fn abajo_entra_por_encima_de_la_barra_de_estado() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(3), KindId::new("tasks")),
                Node::slot(SlotId(4), KindId::new("status")),
            ],
            sizes: vec![Size::Weight(1), Size::Auto, Size::Fixed(1)],
        };
        let con = arbol.dock(
            SlotId(1),
            Edge::Bottom,
            Size::Fixed(12),
            &Node::slot(SlotId(9), KindId::new("log")),
        );
        let Node::Split {
            children, sizes, ..
        } = &con
        else {
            panic!("split")
        };
        let ids: Vec<_> = children.iter().filter_map(Node::first_slot_id).collect();
        assert_eq!(ids, [SlotId(1), SlotId(9), SlotId(3), SlotId(4)]);
        assert_eq!(sizes[1], Size::Fixed(12), "el tamaño va con su hijo");
    }

    /// Los paneles de un mismo borde se AGRUPAN en pestañas (spec 2026-09-21,
    /// fase F): el segundo panel a la derecha no abre otra columna, se une a
    /// la del primero y queda delante; el tamaño es el del grupo. Un listado
    /// no se agrupa nunca, y cerrar una pestaña deshace el grupo.
    #[test]
    fn los_paneles_de_un_mismo_borde_se_agrupan_en_pestanas() {
        let cuerpo = Node::Split {
            dir: Dir::Horizontal,
            children: vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
            sizes: vec![Size::Weight(1), Size::Weight(1)],
        };
        let hoja = |id: u32, k: &str| Node::slot(SlotId(id), KindId::new(k));
        // El primero: una columna, como siempre (su vecino es un listado).
        let uno = cuerpo.dock_grouped(
            SlotId(1),
            Edge::Right,
            Size::Fixed(30),
            &hoja(7, "timeline"),
        );
        let Node::Split { children, .. } = &uno else {
            panic!("split")
        };
        assert_eq!(children.len(), 3);
        // El segundo: pestaña del primero, delante, y el grupo toma el
        // sitio del que más pide — el visor es proporcional, y en las
        // treinta columnas del primero no se leería.
        let dos = uno.dock_grouped(SlotId(1), Edge::Right, Size::Weight(1), &hoja(9, "viewer"));
        let Node::Split {
            children, sizes, ..
        } = &dos
        else {
            panic!("split")
        };
        assert_eq!(children.len(), 3, "no abre otra columna");
        assert_eq!(
            dos.tabs_of(SlotId(9)),
            Some((vec![SlotId(7), SlotId(9)], 1))
        );
        assert_eq!(sizes[2], Size::Weight(1));
        // El tercero se une al mismo grupo, y un fijo no le quita el peso.
        let tres = dos.dock_grouped(
            SlotId(1),
            Edge::Right,
            Size::Fixed(30),
            &hoja(8, "metadata"),
        );
        assert_eq!(
            tres.tabs_of(SlotId(8)),
            Some((vec![SlotId(7), SlotId(9), SlotId(8)], 2))
        );
        // Abajo, por encima de la barra de estado, igual.
        let raiz = Node::Split {
            dir: Dir::Vertical,
            children: vec![tres, hoja(4, "status")],
            sizes: vec![Size::Weight(1), Size::Fixed(1)],
        };
        let r = raiz
            .dock_grouped(SlotId(1), Edge::Bottom, Size::Fixed(12), &hoja(10, "log"))
            .dock_grouped(
                SlotId(1),
                Edge::Bottom,
                Size::Fixed(8),
                &hoja(11, "processes"),
            );
        assert_eq!(
            r.tabs_of(SlotId(11)),
            Some((vec![SlotId(10), SlotId(11)], 1))
        );
        // Dos fijos: el mayor, que el registro no encoja a las ocho filas
        // de los procesos.
        let Node::Split { sizes, .. } = &r else {
            panic!("split")
        };
        assert_eq!(sizes[1], Size::Fixed(12));
        // Cerrar una pestaña de un grupo de dos lo deshace: vuelve la hoja.
        let cerrado = r.close_slot(SlotId(11)).expect("cerrable");
        assert_eq!(cerrado.tabs_of(SlotId(10)), None);
        // Y `dock` a secas sigue sin agrupar: es el de las disposiciones
        // escritas a mano y los presets.
        let suelto = dos.dock(
            SlotId(1),
            Edge::Right,
            Size::Fixed(30),
            &hoja(8, "metadata"),
        );
        assert_eq!(suelto.tabs_of(SlotId(8)), None);
    }

    /// Sin ancestro en el eje pedido, se ENVUELVE. Un solo pane es el caso
    /// real: tras cerrar uno, el cuerpo puede ser una hoja suelta.
    #[test]
    fn sin_ancestro_en_el_eje_se_envuelve() {
        let arbol = Node::slot(SlotId(1), KindId::browser());
        let con = arbol.dock(
            SlotId(1),
            Edge::Left,
            Size::Fixed(16),
            &Node::slot(SlotId(9), KindId::new("places")),
        );
        let Node::Split {
            dir,
            children,
            sizes,
        } = &con
        else {
            panic!("envuelto en Split")
        };
        assert_eq!(*dir, Dir::Horizontal);
        assert_eq!(children[0].first_slot_id(), Some(SlotId(9)));
        assert_eq!(children[1].first_slot_id(), Some(SlotId(1)));
        assert_eq!(sizes, &vec![Size::Fixed(16), Size::Weight(1)]);
    }

    /// Un ancla dentro de una `Tabs` acopla FUERA del grupo: un sidebar que
    /// desaparece al cambiar de pestaña no es un sidebar.
    #[test]
    fn con_el_ancla_en_una_pestana_el_acople_va_fuera_del_grupo() {
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::Tabs {
                    children: vec![
                        Node::slot(SlotId(1), KindId::browser()),
                        Node::slot(SlotId(3), KindId::browser()),
                    ],
                    active: 0,
                },
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let con = arbol.dock(
            SlotId(1),
            Edge::Left,
            Size::Fixed(16),
            &Node::slot(SlotId(9), KindId::new("places")),
        );
        let Node::Split { children, .. } = &con else {
            panic!("split")
        };
        assert_eq!(children.len(), 3);
        assert_eq!(children[0].first_slot_id(), Some(SlotId(9)));
        assert!(matches!(children[1], Node::Tabs { .. }));
    }

    /// Un ancla que no está en el árbol no inventa nada.
    #[test]
    fn un_ancla_que_no_existe_deja_el_arbol_intacto() {
        let arbol = Node::slot(SlotId(1), KindId::browser());
        let con = arbol.dock(
            SlotId(77),
            Edge::Left,
            Size::Fixed(16),
            &Node::slot(SlotId(9), KindId::new("places")),
        );
        assert_eq!(con, arbol);
    }

    /// Y deshacerlo es `close_slot`, que ya existe: el `Split` de un solo hijo
    /// se disuelve y el árbol vuelve a ser el de antes. Es lo que hace que el
    /// toggle sea reversible de verdad y no deje un Split degenerado por cada
    /// vez que alguien abrió y cerró el sidebar.
    #[test]
    fn undock_es_close_slot_y_devuelve_el_arbol_de_antes() {
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let con = arbol.dock(
            SlotId(1),
            Edge::Left,
            Size::Fixed(16),
            &Node::slot(SlotId(9), KindId::new("places")),
        );
        assert_eq!(con.close_slot(SlotId(9)), Some(arbol));
    }

    /// El ancho del primer hijo de un `Split`, para los tests de `resize`.
    fn ancho(n: &Node) -> Size {
        match n {
            Node::Split { sizes, .. } => sizes[0],
            _ => panic!("split"),
        }
    }

    fn con_sidebar(ancho: u16) -> Node {
        Node::Split {
            dir: Dir::Horizontal,
            children: vec![
                Node::slot(SlotId(5), KindId::new("places")),
                Node::slot(SlotId(1), KindId::browser()),
            ],
            sizes: vec![Size::Fixed(ancho), Size::Weight(1)],
        }
    }

    /// #227: un hijo FIJO —el ancho del sidebar— se mueve en CELDAS. Antes
    /// `resize` solo tocaba pesos, así que el panel de sitios no se podía
    /// ensanchar con el teclado y los presets con sidebar nacían atascados.
    #[test]
    fn un_hijo_fijo_se_mueve_en_celdas() {
        let arbol = con_sidebar(16);
        assert_eq!(ancho(&arbol.resize(SlotId(5), 1)), Size::Fixed(18));
        assert_eq!(ancho(&arbol.resize(SlotId(5), -1)), Size::Fixed(14));
    }

    /// El tope de abajo existe para que no se pueda dejar en cero: un panel de
    /// ancho cero no se ve y no hay forma de volver a agrandarlo.
    #[test]
    fn un_hijo_fijo_no_baja_de_dos_ni_pasa_de_cien() {
        assert_eq!(ancho(&con_sidebar(2).resize(SlotId(5), -1)), Size::Fixed(2));
        assert_eq!(
            ancho(&con_sidebar(100).resize(SlotId(5), 1)),
            Size::Fixed(100)
        );
    }

    /// Y un hijo PONDERADO sigue haciendo exactamente lo de antes.
    #[test]
    fn un_hijo_ponderado_no_cambia_de_comportamiento() {
        let arbol = Node::split(
            Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        assert_eq!(ancho(&arbol.resize(SlotId(1), 1)), Size::Weight(2));
        assert_eq!(ancho(&arbol.resize(SlotId(1), -1)), Size::Weight(1));
    }

    /// El `orthodox` de siempre: dos listados lado a lado sobre las filas de
    /// cromo.
    fn ortodoxo() -> Node {
        Node::Split {
            dir: Dir::Vertical,
            children: vec![
                Node::split(Dir::Horizontal, vec![b(1), b(2)]),
                Node::slot(SlotId(3), KindId::new("tasks")),
                Node::slot(SlotId(4), KindId::new("status")),
            ],
            sizes: vec![Size::Weight(1), Size::Auto, Size::Fixed(1)],
        }
    }

    /// ADR 0138: soltar a un lado reparte con el destino a partes iguales, y
    /// en el centro se une como pestaña. Nada se pierde.
    #[test]
    fn mover_un_hueco_lo_suelta_al_lado_o_como_pestana() {
        let t = ortodoxo();
        // El 1 debajo del 2: el reparto horizontal se disuelve, y el 2 se
        // parte en vertical DENTRO de su sitio — no como hermano en la raíz,
        // que es la del cromo y no se gira: así `flip` lo deshace.
        let abajo = t.move_slot(SlotId(1), SlotId(2), DropZone::Bottom);
        let Node::Split { children, .. } = &abajo else {
            panic!("raíz")
        };
        assert_eq!(
            children[0],
            Node::Split {
                dir: Dir::Vertical,
                children: vec![b(2), b(1)],
                sizes: vec![Size::Weight(1); 2],
            }
        );
        assert_eq!(children.len(), 3, "el cromo sigue igual");
        assert_eq!(
            abajo.flip(SlotId(1)),
            t.move_slot(SlotId(1), SlotId(2), DropZone::Right),
            "girar lo que se soltó abajo es soltarlo a la derecha"
        );
        // Un tercero a la derecha del 1 entra como HERMANO: tercios.
        let tres = Node::split(Dir::Horizontal, vec![b(1), b(2), b(5)]);
        let movido = tres.move_slot(SlotId(5), SlotId(1), DropZone::Right);
        let Node::Split {
            children, sizes, ..
        } = &movido
        else {
            panic!("split")
        };
        assert_eq!(children, &vec![b(1), b(5), b(2)]);
        assert_eq!(sizes, &vec![Size::Weight(1); 3]);
        // En el centro: pestaña del destino, delante.
        let centro = t.move_slot(SlotId(1), SlotId(2), DropZone::Center);
        assert_eq!(
            centro.tabs_of(SlotId(1)),
            Some((vec![SlotId(2), SlotId(1)], 1))
        );
        // Los mismos huecos, siempre.
        for m in [&abajo, &movido, &centro] {
            let mut ids = m.slot_ids();
            ids.sort_unstable();
            assert!(m.duplicate_slot_ids().is_empty());
            assert!(ids.windows(2).all(|w| w[0] != w[1]));
        }
    }

    /// Soltar al lado de una pestaña parte el GRUPO, no lo invade.
    #[test]
    fn soltar_junto_a_una_pestana_parte_su_grupo() {
        let grupo = Node::Tabs {
            children: vec![b(1), b(2)],
            active: 0,
        };
        let t = Node::split(Dir::Horizontal, vec![grupo.clone(), b(3)]);
        let m = t.move_slot(SlotId(3), SlotId(2), DropZone::Top);
        assert_eq!(
            m,
            Node::Split {
                dir: Dir::Vertical,
                children: vec![b(3), grupo],
                sizes: vec![Size::Weight(1); 2],
            }
        );
    }

    /// Lo que no se mueve: a sí mismo, el cromo, el único hueco, un id que
    /// no está.
    #[test]
    fn mover_lo_que_no_se_mueve_no_cambia_nada() {
        let t = ortodoxo();
        assert_eq!(t.move_slot(SlotId(1), SlotId(1), DropZone::Left), t);
        assert_eq!(t.move_slot(SlotId(4), SlotId(1), DropZone::Top), t);
        assert_eq!(t.move_slot(SlotId(1), SlotId(4), DropZone::Top), t);
        assert_eq!(t.move_slot(SlotId(9), SlotId(1), DropZone::Top), t);
        assert_eq!(b(1).move_slot(SlotId(1), SlotId(2), DropZone::Left), b(1));
    }

    /// La zona bajo el puntero, en celdas: la regla del cuarto, como la
    /// ventana; y la parte que se resalta.
    #[test]
    fn la_zona_de_soltar_sale_del_cuarto_mas_cercano() {
        let r = Rect {
            x: 10,
            y: 0,
            width: 40,
            height: 20,
        };
        assert_eq!(DropZone::at(11, 10, r), DropZone::Left);
        assert_eq!(DropZone::at(48, 10, r), DropZone::Right);
        assert_eq!(DropZone::at(30, 1, r), DropZone::Top);
        assert_eq!(DropZone::at(30, 18, r), DropZone::Bottom);
        assert_eq!(DropZone::at(30, 10, r), DropZone::Center);
        assert_eq!(
            DropZone::Right.part_of(r),
            Rect {
                x: 30,
                y: 0,
                width: 20,
                height: 20
            }
        );
        assert_eq!(DropZone::Center.part_of(r), r);
    }

    /// Junto a un panel de ancho fijo, lo soltado entra como HERMANO con
    /// peso: partir el fijo por dentro le daría ocho columnas a un listado.
    #[test]
    fn soltar_junto_a_un_fijo_entra_como_hermano() {
        let t = Node::Split {
            dir: Dir::Horizontal,
            children: vec![Node::slot(SlotId(7), KindId::new("places")), b(1), b(2)],
            sizes: vec![Size::Fixed(16), Size::Weight(1), Size::Weight(1)],
        };
        let m = t.move_slot(SlotId(2), SlotId(7), DropZone::Right);
        let Node::Split {
            sizes, children, ..
        } = &m
        else {
            panic!("split")
        };
        assert_eq!(sizes[0], Size::Fixed(16), "los sitios conservan su ancho");
        assert_eq!(children[1], b(2));
        assert!(matches!(sizes[1], Size::Weight(_)));
    }

    /// El centro solo junta familias iguales (ADR 0134): un listado no
    /// entra en las pestañas de los sitios, ni un panel en las de un
    /// listado.
    #[test]
    fn el_centro_no_mezcla_listados_y_paneles() {
        let t = Node::Split {
            dir: Dir::Horizontal,
            children: vec![Node::slot(SlotId(7), KindId::new("places")), b(1), b(2)],
            sizes: vec![Size::Fixed(16), Size::Weight(1), Size::Weight(1)],
        };
        assert_eq!(t.move_slot(SlotId(1), SlotId(7), DropZone::Center), t);
        assert_eq!(t.move_slot(SlotId(7), SlotId(1), DropZone::Center), t);
        assert_ne!(t.move_slot(SlotId(1), SlotId(2), DropZone::Center), t);
    }

    /// ADR 0138: girar pasa lado a lado a uno encima del otro y vuelve; no
    /// gira el reparto del cromo, y un fijo pasa a peso.
    #[test]
    fn girar_cambia_el_eje_del_reparto_interior() {
        let t = ortodoxo();
        let g = t.flip(SlotId(1));
        let Node::Split { children, .. } = &g else {
            panic!("raíz")
        };
        assert!(matches!(
            &children[0],
            Node::Split {
                dir: Dir::Vertical,
                ..
            }
        ));
        assert_eq!(g.flip(SlotId(2)), t, "girar dos veces es no girar");
        // Un solo listado sobre el cromo: nada que girar.
        let solo = Node::Split {
            dir: Dir::Vertical,
            children: vec![b(1), Node::slot(SlotId(4), KindId::new("status"))],
            sizes: vec![Size::Weight(1), Size::Fixed(1)],
        };
        assert_eq!(solo.flip(SlotId(1)), solo);
        // Solo la RACHA ponderada: los sitios siguen siendo una columna de
        // dieciséis, y los dos listados se apilan a su lado.
        let con_sitios = Node::Split {
            dir: Dir::Horizontal,
            children: vec![Node::slot(SlotId(7), KindId::new("places")), b(1), b(2)],
            sizes: vec![Size::Fixed(16), Size::Weight(1), Size::Weight(1)],
        };
        let g = con_sitios.flip(SlotId(1));
        assert_eq!(
            g,
            Node::Split {
                dir: Dir::Horizontal,
                children: vec![
                    Node::slot(SlotId(7), KindId::new("places")),
                    Node::split(Dir::Vertical, vec![b(1), b(2)]),
                ],
                sizes: vec![Size::Fixed(16), Size::Weight(2)],
            }
        );
        // Y de vuelta: los sitios no pierden su ancho.
        let Node::Split { sizes, .. } = g.flip(SlotId(2)) else {
            panic!("split")
        };
        assert_eq!(sizes[0], Size::Fixed(16));
        // Una racha de uno no se gira, y la negativa NO sube a girar el
        // reparto de fuera.
        let solo_uno = Node::Split {
            dir: Dir::Horizontal,
            children: vec![Node::slot(SlotId(7), KindId::new("places")), b(1)],
            sizes: vec![Size::Fixed(30), Size::Weight(1)],
        };
        assert_eq!(solo_uno.flip(SlotId(1)), solo_uno);
        let anidado = Node::split(Dir::Vertical, vec![solo_uno.clone(), b(9)]);
        assert_eq!(anidado.flip(SlotId(1)), anidado);
    }
}
