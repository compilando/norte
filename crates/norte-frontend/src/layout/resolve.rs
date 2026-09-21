//! El reparto: árbol + área → quién se pinta, dónde, y quién no.

use super::{Dir, KindRegistry, LayoutDiagnostic, Node, Rect, RoleId, Size, SlotId};

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
    // #229: una pantalla sin un listado USABLE no es una pantalla, es un
    // cuelgue con bordes. `layout.close-slot` ya se niega a cerrar el último
    // listado por esta razón; el reparto podía producir exactamente eso, y esta
    // es la otra mitad de la misma regla.
    //
    // Pasa porque los FIJOS cobran enteros y antes que los pesos, y un `Split`
    // con un hijo fijo no colapsa (colapsarlo se llevaría el cromo). Con
    // `full` a 40x10 eso son 16 del sidebar más 30 de la columna derecha sobre
    // 40 columnas: los dos browsers a cero.
    if listado_usable(&out, tree, decls) || !tiene_listado(tree, decls) {
        return out;
    }
    // El cromo se aparta DEL MÁS GRANDE AL MÁS PEQUEÑO y de uno en uno, y se
    // para en el primer intento que sí enseña un listado: así la barra de
    // estado —un fijo de una fila— sobrevive a que se aparte un panel de ocho.
    // El árbol de entrada NO se toca: esto vive y muere con el frame, como el
    // colapso (ADR 0058 D5).
    let mut podado = tree.clone();
    let mut apartados: Vec<SlotId> = Vec::new();
    let mut corto = ejes_cortos(&out, tree, decls);
    // El más grande DE UN EJE CORTO, uno por vuelta. Los candidatos se
    // recalculan sobre el árbol ya podado —apartar un hijo mueve los índices de
    // sus hermanos— y los ejes también: apartar un panel ancho puede dejar el
    // ancho resuelto y el alto no. Cada vuelta quita uno, así que termina.
    while let Some(elegido) = cromo_de_mayor_a_menor(&podado, decls)
        .into_iter()
        .find(|c| match c.eje {
            Dir::Horizontal => corto.0,
            Dir::Vertical => corto.1,
        })
    {
        let Some(mas_pequeno) = sin_camino(&podado, &elegido.camino) else {
            break;
        };
        podado = mas_pequeno;
        apartados.extend(elegido.slots.iter().copied());
        let mut intento = Resolved::default();
        place(&podado, area, decls, &mut intento);
        if listado_usable(&intento, tree, decls) {
            for id in &apartados {
                intento
                    .diagnostics
                    .push(LayoutDiagnostic::ChromeSetAside { slot: *id });
            }
            // Apartado es SUSPENDIDO, no perdido: `placements` y `hidden`
            // parten los huecos del árbol, y un panel que no se pinta y
            // tampoco se suspende se queda con su watch abierto.
            intento.hidden.extend(apartados);
            return intento;
        }
        corto = ejes_cortos(&intento, tree, decls);
    }
    // Apartar todo el cromo tampoco lo arregla: se queda el reparto de verdad,
    // que al menos respeta lo que el usuario fijó. Cambiarlo por una pantalla
    // igual de inútil y además sin cromo no le sirve a nadie.
    out
}

/// El TOPE del suelo de «esto enseña algo»: doce columnas y cuatro filas.
///
/// Doce columnas es lo que ocupa un nombre corto entre bordes —un `browser` de
/// 12x8 está apretado y sirve, y por eso el preset `krusader` a 40x10 conserva
/// su sidebar y esta regla ni se entera—; cuatro filas son dos bordes, la
/// cabecera y una entrada. Un panel por debajo de esto no está apretado: está
/// vacío.
///
/// No es el mínimo del kind, y la diferencia es la que separa «apretado» de
/// «vacío». El mínimo de un `browser` son 20x5 y dice cuándo un reparto
/// PROPORCIONAL colapsa —«por debajo de esto no pinto un nombre con su
/// tamaño»—; esto dice cuándo el panel no pinta NADA.
///
/// Es un TOPE y no el número final: el suelo de verdad lo da
/// [`minimo_visible`], que parte del mínimo del kind. Así un kind modesto no
/// tiene que fingir que necesita doce columnas, y uno exigente —el registro es
/// abierto— no arrastra el rescate a apartar cromo persiguiendo un tamaño
/// imposible.
const CONTENIDO: (u16, u16) = (12, 4);

/// El suelo de un kind: su propio mínimo, TOPADO por [`CONTENIDO`].
///
/// El tope es lo que impide que un kind exigente —el registro es abierto y
/// `insert` es público, así que un plugin puede declarar `min = (40, 8)`— haga
/// que el rescate aparte cromo eternamente persiguiendo un tamaño que la
/// pantalla no tiene. El mínimo propio es lo que impide lo contrario: dar por
/// «usable» a 4 columnas algo que declaró necesitar cuarenta.
fn minimo_visible(decls: &KindRegistry, kind: &super::KindId) -> (u16, u16) {
    let (mw, mh) = decls.min_of(kind);
    (mw.min(CONTENIDO.0), mh.min(CONTENIDO.1))
}

/// Dónde empieza y cuánto mide, en el eje de `dir`, la pareja de un borde:
/// desde el principio de lo colocado de `izq` hasta el final de lo colocado
/// de `der` (los dos lados de [`Node::border_pair`]). `None` si alguno de
/// los dos lados no tiene nada colocado.
///
/// Una sola cuenta para la TUI y la ventana: medir solo los dos huecos que
/// se tocan —y no los hijos enteros del reparto— daba la fracción de otra
/// pareja, y el borde entre un listado y los detalles no seguía al puntero.
#[must_use]
pub fn border_span(res: &Resolved, izq: &[SlotId], der: &[SlotId], dir: Dir) -> Option<(u16, u16)> {
    let tramo = |ids: &[SlotId]| {
        res.placements
            .iter()
            .filter(|(id, _)| ids.contains(id))
            .map(|(_, r)| match dir {
                Dir::Horizontal => (r.x, r.x + r.width),
                Dir::Vertical => (r.y, r.y + r.height),
            })
            .reduce(|(a0, a1), (b0, b1)| (a0.min(b0), a1.max(b1)))
    };
    let (inicio, _) = tramo(izq)?;
    let (_, fin) = tramo(der)?;
    Some((inicio, fin.saturating_sub(inicio)))
}

/// ¿Sigue a la vista, en el reparto `despues`, todo lo que `antes` enseñaba?
/// (ADR 0138)
///
/// Lo pregunta quien MUEVE o GIRA un panel antes de quedarse con el árbol
/// nuevo: soltar un listado debajo de otro en un terminal bajo lo deja sin
/// sitio, el reparto lo esconde y el lector ve que su panel desapareció.
/// `tolerado` es el que se va detrás de una pestaña a propósito —el destino
/// de soltar en el centro—. Además, si antes se veían dos listados o más,
/// después tienen que verse dos: «el otro panel» es el destino de copiar, y
/// con uno solo a la vista no hay otro.
#[must_use]
pub fn keeps_on_screen(
    antes: &Resolved,
    despues: &Resolved,
    arbol: &Node,
    tolerado: Option<SlotId>,
) -> bool {
    let colocado = |r: &Resolved, id: SlotId| r.placements.iter().any(|(s, _)| *s == id);
    let perdido = antes
        .placements
        .iter()
        .any(|(id, _)| Some(*id) != tolerado && !colocado(despues, *id));
    let listados = |r: &Resolved| {
        r.placements
            .iter()
            .filter(|(id, _)| arbol.kind_of(*id).is_some_and(|k| k.as_str() == "browser"))
            .count()
    };
    !perdido && listados(despues) >= listados(antes).min(2)
}

/// ¿Cabrían DOS huecos de `kind` si se parte `rect` a lo largo de `dir`?
///
/// Lo pregunta quien va a partir, ANTES de tocar el árbol. Sin esto, partir un
/// hueco que ya no da para dos crea un panel que el reparto esconde en el
/// mismo frame: el `Split` no cabe, se degrada a pestañas y la pantalla vuelve
/// a enseñar uno — con el árbol guardando el nuevo igualmente. Lo que el
/// lector ve es una tecla que no hace nada, o peor, que deshace lo anterior.
///
/// La cuenta es la MISMA que decide el colapso —el mínimo declarado del kind,
/// no el de contenido—, y por eso vive aquí al lado: dos criterios para la
/// misma pregunta se separan en cuanto alguien toque uno.
#[must_use]
pub fn has_room_to_split(rect: Rect, dir: Dir, kind: &super::KindId, decls: &KindRegistry) -> bool {
    let (mw, mh) = decls.min_of(kind);
    match dir {
        Dir::Horizontal => rect.width / 2 >= mw && rect.height >= mh,
        Dir::Vertical => rect.height / 2 >= mh && rect.width >= mw,
    }
}

/// ¿Hay en `out` un hueco que pueda tomar el rol `active` y con sitio para
/// enseñar algo?
fn listado_usable(out: &Resolved, tree: &Node, decls: &KindRegistry) -> bool {
    out.placements.iter().any(|(id, re)| {
        tree.kind_of(*id).is_some_and(|k| {
            let (mw, mh) = minimo_visible(decls, k);
            decls.holds_role(k, RoleId::Active) && re.width >= mw && re.height >= mh
        })
    })
}

/// En qué EJES se queda corto el mejor listado de `out`: `(ancho, alto)`.
///
/// Se mira por eje y no en bloque porque el cromo también es de un eje: un
/// panel fijo dentro de un `Split` vertical se come alto y no ancho, y
/// apartarlo no arregla un ancho corto. Sin esto, un `full` a 80x10 —donde solo
/// falta ALTO— perdía además el sidebar y la columna derecha, que no estorbaban.
fn ejes_cortos(out: &Resolved, tree: &Node, decls: &KindRegistry) -> (bool, bool) {
    // Un eje se ataca si le falta a ALGUNO, no si les falta a todos.
    //
    // Esta función solo se llama cuando NINGÚN listado sirve, así que la
    // pregunta no es «¿están todos estrechos?» sino «¿a cuál de los dos ejes le
    // puedo dar sitio para que alguno sirva?». Con el «todos» fallaba una
    // pantalla real: un listado ancho y de una fila arriba, y otro alto y de
    // dos columnas abajo. Ninguno servía, cada uno cojeaba de un eje distinto,
    // y como no TODOS eran estrechos ni TODOS bajos, el rescate contestaba que
    // no faltaba ningún eje y devolvía la pantalla rota intacta.
    let listados = || {
        out.placements.iter().filter_map(|(id, re)| {
            let k = tree.kind_of(*id)?;
            decls
                .holds_role(k, RoleId::Active)
                .then(|| (minimo_visible(decls, k), re))
        })
    };
    let mut falta = (false, false);
    let mut hay = false;
    for ((mw, mh), re) in listados() {
        hay = true;
        falta.0 |= re.width < mw;
        falta.1 |= re.height < mh;
    }
    // Sin ningún listado colocado, los dos ejes están en juego: lo que falta es
    // sitio, y no se sabe de cuál.
    if hay { falta } else { (true, true) }
}

/// ¿Tiene el árbol algún hueco que pueda tomar el rol `active`?
///
/// Un layout que no lo tiene —una pantalla de solo cromo— es legal, y para él
/// no hay nada que rescatar: se reparte y se pinta.
fn tiene_listado(node: &Node, decls: &KindRegistry) -> bool {
    match node {
        Node::Slot { kind, .. } => decls.holds_role(kind, RoleId::Active),
        Node::Split { children, .. } | Node::Tabs { children, .. } => {
            children.iter().any(|c| tiene_listado(c, decls))
        }
    }
}

/// El cromo del árbol, del más grande al más pequeño.
///
/// **Cromo = hijo con tamaño `Fixed` cuyo subárbol no tiene ni un hueco que
/// pueda tomar `active`.** Las dos mitades importan. Fijo, porque un ponderado
/// ya cede sitio solo. Y sin listado dentro, porque un fijo que ES un listado
/// no es cromo: apartarlo sería quitar una pantalla para dársela a otra.
///
/// No se entra en un hijo que ya es cromo: se aparta entero, y sus fijos
/// interiores no son decisiones separadas.
fn cromo_de_mayor_a_menor(node: &Node, decls: &KindRegistry) -> Vec<Cromo> {
    let mut fuera = Vec::new();
    recoge_cromo(node, decls, &[], &mut fuera);
    // Empate resuelto por los ids: el reparto de un frame no puede depender
    // del orden en que un `sort_unstable` deje dos panes del mismo tamaño.
    fuera.sort_by(|a, b| {
        b.declarado
            .cmp(&a.declarado)
            .then_with(|| a.slots.cmp(&b.slots))
    });
    fuera
}

/// Un panel acoplado que se puede apartar, y por qué eje se come el sitio.
#[derive(Debug, Clone)]
struct Cromo {
    /// Las celdas que declara en el eje de su padre.
    declarado: u16,
    /// El eje del `Split` que lo contiene: el sitio que devuelve al apartarlo.
    eje: Dir,
    /// Los índices de hijo desde la raíz hasta él.
    ///
    /// Por POSICIÓN y no por sus `SlotId`, y no es un detalle: un árbol puede
    /// traer el mismo id dos veces —[`super::validate`] lo rechaza, pero
    /// `resolve` tiene que aguantar cualquier árbol— y apartar «los huecos con
    /// este id» se llevaba también la otra copia, que se quedaba sin pintar y
    /// sin suspender. Un hueco vivo que nadie pinta y nadie suspende es un
    /// watch abierto mirando a nada, y lo cazó la propiedad de partición.
    camino: Vec<usize>,
    /// Los huecos que se lleva.
    slots: Vec<SlotId>,
}

/// Acumula el cromo de `node` en `fuera`, arrastrando el camino desde la raíz.
fn recoge_cromo(node: &Node, decls: &KindRegistry, aqui: &[usize], fuera: &mut Vec<Cromo>) {
    let (Node::Split { children, .. } | Node::Tabs { children, .. }) = node else {
        return;
    };
    // Un `Tabs` no reparte sitio: sus hijos lo comparten, así que ninguno es
    // cromo por tamaño y aquí solo se baja a mirar.
    let eje = if let Node::Split { dir, .. } = node {
        *dir
    } else {
        Dir::Horizontal
    };
    for (i, c) in children.iter().enumerate() {
        let mut camino = aqui.to_vec();
        camino.push(i);
        match (sizes_get(node, i), tiene_listado(c, decls)) {
            (Some(Size::Fixed(n)), false) => fuera.push(Cromo {
                declarado: n,
                eje,
                camino,
                slots: c.slot_ids(),
            }),
            _ => recoge_cromo(c, decls, &camino, fuera),
        }
    }
}

/// El tamaño del hijo `i`, si su padre reparte por tamaños. Un `Tabs` no
/// reparte: sus hijos comparten el sitio, así que ninguno es cromo por tamaño.
fn sizes_get(node: &Node, i: usize) -> Option<Size> {
    match node {
        Node::Split { sizes, .. } => sizes.get(i).copied(),
        Node::Slot { .. } | Node::Tabs { .. } => None,
    }
}

/// El árbol sin el hijo que `camino` señala, o `None` si no queda nada.
///
/// Privada a propósito, y no un método de [`Node`]: los de allí son
/// intenciones del usuario y se PERSISTEN. Esto es un apaño de un frame, y
/// tenerlo a mano donde se guarda un layout es cómo el tamaño del terminal
/// acaba borrándole el sidebar a alguien para siempre.
fn sin_camino(node: &Node, camino: &[usize]) -> Option<Node> {
    let Some((&i, resto)) = camino.split_first() else {
        // Camino agotado: este nodo ES el que se va.
        return None;
    };
    match node {
        // Un camino que atraviesa un hueco no existe; el árbol se queda igual.
        Node::Slot { .. } => Some(node.clone()),
        Node::Split {
            dir,
            children,
            sizes,
        } => {
            let mut hijos = Vec::new();
            let mut tam = Vec::new();
            for (j, c) in children.iter().enumerate() {
                let queda = if j == i {
                    sin_camino(c, resto)
                } else {
                    Some(c.clone())
                };
                if let Some(q) = queda {
                    hijos.push(q);
                    tam.push(sizes.get(j).copied().unwrap_or(Size::Weight(1)));
                }
            }
            if hijos.is_empty() {
                return None;
            }
            Some(Node::Split {
                dir: *dir,
                children: hijos,
                sizes: tam,
            })
        }
        Node::Tabs { children, active } => {
            let mut hijos = Vec::new();
            // La pestaña activa es un ÍNDICE: quitar una de delante mueve a
            // todas las de detrás. Clamparlo sin más dejaba a la vista la
            // siguiente y SUSPENDÍA la que el usuario estaba mirando.
            let mut activo = *active;
            for (j, c) in children.iter().enumerate() {
                let queda = if j == i {
                    sin_camino(c, resto)
                } else {
                    Some(c.clone())
                };
                if queda.is_none() && j < activo {
                    activo -= 1;
                }
                if let Some(q) = queda {
                    hijos.push(q);
                }
            }
            if hijos.is_empty() {
                return None;
            }
            Some(Node::Tabs {
                active: activo.min(hijos.len() - 1),
                children: hijos,
            })
        }
    }
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
        Node::Split {
            dir,
            children,
            sizes,
        } => children
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let (cw, ch) = min_of(c, decls);
                // Un hijo FIJO no pide su mínimo a lo largo del eje: pide
                // exactamente lo que declara, y eso gana.
                match (sizes.get(i), dir) {
                    (Some(Size::Fixed(n)), Dir::Horizontal) => (*n, ch),
                    (Some(Size::Fixed(n)), Dir::Vertical) => (cw, *n),
                    (Some(Size::Auto), Dir::Horizontal) => (0, ch),
                    (Some(Size::Auto), Dir::Vertical) => (cw, 0),
                    _ => (cw, ch),
                }
            })
            .fold((0, 0), |(w, h), (cw, ch)| match dir {
                Dir::Horizontal => (w.saturating_add(cw), h.max(ch)),
                Dir::Vertical => (w.max(cw), h.saturating_add(ch)),
            }),
    }
}

/// Reparte `area` entre `sizes` a lo largo de `dir`.
///
/// Los FIJOS cobran primero y en orden; si ya no caben, se recortan y los
/// pesos se quedan sin nada — que es lo que hace que un terminal diminuto siga
/// pintando la barra de estado en vez de la nada. El resto se reparte entre
/// los pesos, y el sobrante de la división entera va al ÚLTIMO PONDERADO: con
/// un ancho impar se perdería una columna, y una columna sin pintar en el
/// borde de un pane se ve.
///
/// Un [`Size::Auto`] cuenta como cero. No debería llegar hasta aquí
/// —[`Node::substitute_auto`] lo sustituye antes— pero un reparto no es sitio
/// para reventar.
///
/// `suelos` es el mínimo de cada hijo en el eje del reparto. Si el sitio
/// libre alcanza para el de TODOS los ponderados, ninguno baja del suyo: se
/// le da su mínimo y el resto se vuelve a repartir entre los demás, en su
/// proporción. Si no alcanza, el reparto es el proporcional de siempre y el
/// colapso decide. La columna sobrante de la división entera va al último
/// ponderado de los que NO se quedaron en su suelo. Sin esto un peso pequeño al lado de pesos grandes —el que
/// deja arrastrar un borde— salía de un píxel.
fn distribute(area: Rect, dir: Dir, sizes: &[Size], suelos: &[u16]) -> Vec<Rect> {
    let extent = u64::from(match dir {
        Dir::Horizontal => area.width,
        Dir::Vertical => area.height,
    });
    let mut asignado: Vec<u64> = vec![0; sizes.len()];
    let mut usado: u64 = 0;
    for (i, s) in sizes.iter().enumerate() {
        if let Size::Fixed(n) = s {
            let cabe = u64::from(*n).min(extent.saturating_sub(usado));
            asignado[i] = cabe;
            usado = usado.saturating_add(cabe);
        }
    }
    let resto = extent.saturating_sub(usado);
    let pesos: Vec<u64> = sizes
        .iter()
        .map(|s| match s {
            Size::Weight(w) => u64::from((*w).max(1)),
            Size::Fixed(_) | Size::Auto => 0,
        })
        .collect();
    let suelo = |i: usize| u64::from(suelos.get(i).copied().unwrap_or(0));
    let caben_los_suelos = (0..sizes.len())
        .filter(|i| pesos[*i] > 0)
        .map(suelo)
        .sum::<u64>()
        <= resto;
    // Los ponderados que ya cobran su suelo salen del reparto proporcional.
    let mut al_suelo = vec![false; sizes.len()];
    loop {
        let fijo: u64 = (0..sizes.len()).filter(|i| al_suelo[*i]).map(suelo).sum();
        let libre = resto.saturating_sub(fijo);
        let activos: Vec<usize> = (0..sizes.len())
            .filter(|i| pesos[*i] > 0 && !al_suelo[*i])
            .collect();
        let total: u64 = activos.iter().map(|i| pesos[*i]).sum();
        let mut dado: u64 = 0;
        for (n, &i) in activos.iter().enumerate() {
            asignado[i] = if n + 1 == activos.len() {
                libre.saturating_sub(dado)
            } else {
                libre
                    .saturating_mul(pesos[i])
                    .checked_div(total)
                    .unwrap_or(0)
            };
            dado = dado.saturating_add(asignado[i]);
        }
        for i in 0..sizes.len() {
            if al_suelo[i] {
                asignado[i] = suelo(i);
            }
        }
        if !caben_los_suelos {
            break;
        }
        let bajos: Vec<usize> = activos
            .iter()
            .copied()
            .filter(|i| asignado[*i] < suelo(*i))
            .collect();
        // Cada vuelta fija al menos uno más, así que termina.
        if bajos.is_empty() {
            break;
        }
        for i in bajos {
            al_suelo[i] = true;
        }
    }
    let mut out = Vec::with_capacity(sizes.len());
    let mut off: u64 = 0;
    for size in asignado {
        let o = u16::try_from(off).unwrap_or(u16::MAX);
        let s16 = u16::try_from(size).unwrap_or(u16::MAX);
        out.push(match dir {
            Dir::Horizontal => Rect::new(area.x.saturating_add(o), area.y, s16, area.height),
            Dir::Vertical => Rect::new(area.x, area.y.saturating_add(o), area.width, s16),
        });
        off = off.saturating_add(size);
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
            sizes,
        } => {
            let tam: Vec<Size> = (0..children.len())
                .map(|i| match sizes.get(i) {
                    Some(Size::Weight(0)) => {
                        out.diagnostics
                            .push(LayoutDiagnostic::ZeroWeightRaised { at: i });
                        Size::Weight(1)
                    }
                    Some(otro) => *otro,
                    None => Size::Weight(1),
                })
                .collect();
            let suelos: Vec<u16> = children
                .iter()
                .map(|c| {
                    let (mw, mh) = min_of(c, decls);
                    match dir {
                        Dir::Horizontal => mw,
                        Dir::Vertical => mh,
                    }
                })
                .collect();
            let rects = distribute(area, *dir, &tam, &suelos);
            // Un `Split` colapsa SOLO si todos sus hijos son ponderados.
            //
            // Colapsar es «estos hermanos se disputan el mismo eje y no caben,
            // así que enseña uno». En cuanto hay un hijo fijo, el reparto ya no
            // es una disputa: es cromo acoplado más una zona flexible, y
            // colapsarlo se llevaría por delante el cromo. Con el preset
            // `orthodox` eso sería, literalmente, quedarse sin barra de estado
            // en un terminal bajo. Lo que sí colapsa es la zona flexible por su
            // cuenta, cuando le toque.
            let cabe = !tam.iter().all(|s| matches!(s, Size::Weight(_)))
                || children.iter().zip(&rects).all(|(c, r)| {
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
            sizes: vec![Size::Weight(1), Size::Weight(1)],
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

    /// REGRESIÓN (captura del 2026-09-21): un PESO no baja del mínimo de su
    /// kind mientras el sitio libre alcance para todos. El layout guardado
    /// tenía listados a 49/51 y el visor a 1, al lado de fijos: el visor
    /// salía de un píxel, y un `Split` con fijos no colapsa. Ahora toma su
    /// mínimo (20) y los demás pesos ceden la diferencia en proporción.
    #[test]
    fn un_peso_no_baja_de_su_minimo_si_hay_sitio() {
        let arbol = Node::Split {
            dir: Dir::Horizontal,
            children: vec![
                Node::slot(SlotId(5), KindId::new("places")),
                browser(1),
                browser(2),
                Node::slot(SlotId(9), KindId::new("viewer")),
            ],
            sizes: vec![
                Size::Fixed(16),
                Size::Weight(49),
                Size::Weight(51),
                Size::Weight(1),
            ],
        };
        let out = resolve(r(0, 0, 200, 30), &arbol, &reg());
        let ancho = |id: u32| {
            out.placements
                .iter()
                .find(|(s, _)| *s == SlotId(id))
                .map(|(_, re)| re.width)
                .expect("colocado")
        };
        assert_eq!(ancho(9), 20, "el visor, a su mínimo");
        assert_eq!(ancho(5), 16, "el fijo no cede");
        assert_eq!(ancho(1) + ancho(2) + 20 + 16, 200, "nada se pierde");
        assert!(ancho(2) > ancho(1), "los demás conservan su proporción");
        // Sin sitio para todos los mínimos, el reparto es el de siempre.
        let out = resolve(r(0, 0, 60, 30), &arbol, &reg());
        assert!(out.placements.len() + out.hidden.len() >= 4);
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

    /// Preguntar ANTES de partir: en 30 columnas no caben dos browsers de
    /// mínimo 20, así que partir ahí solo produce un panel que el reparto
    /// esconde en el mismo frame.
    ///
    /// La misma cuenta que hace el colapso —el mínimo del KIND, no el de
    /// contenido—, para que la respuesta de los dos no pueda separarse.
    #[test]
    fn has_room_to_split_dice_que_no_cuando_el_reparto_colapsaria() {
        let k = KindId::browser();
        assert!(has_room_to_split(
            r(0, 0, 40, 30),
            Dir::Horizontal,
            &k,
            &reg()
        ));
        assert!(!has_room_to_split(
            r(0, 0, 30, 30),
            Dir::Horizontal,
            &k,
            &reg()
        ));
        // El eje que se parte es el que cuenta: 30 columnas no dan para dos
        // a lo ancho y las mismas 30 filas sí dan para dos a lo alto.
        assert!(has_room_to_split(
            r(0, 0, 30, 30),
            Dir::Vertical,
            &k,
            &reg()
        ));
        assert!(!has_room_to_split(
            r(0, 0, 30, 9),
            Dir::Vertical,
            &k,
            &reg()
        ));
    }

    /// ADR 0138: apilar dos listados donde no caben esconde uno, y eso se
    /// rehúsa; con sitio, no. Juntarlos en pestañas deja uno a la vista, y
    /// con dos antes eso también se rehúsa.
    #[test]
    fn mover_no_puede_dejar_nada_fuera_de_la_vista() {
        use crate::layout::DropZone;
        let b = |id| Node::slot(SlotId(id), KindId::browser());
        let arbol = Node::split(Dir::Horizontal, vec![b(1), b(2)]);
        let apilado = arbol.move_slot(SlotId(1), SlotId(2), DropZone::Bottom);
        let juntos = arbol.move_slot(SlotId(1), SlotId(2), DropZone::Center);
        let bajo = r(0, 0, 100, 8);
        let alto = r(0, 0, 100, 40);
        let ok = |area, nuevo: &Node, tolerado| {
            keeps_on_screen(
                &resolve(area, &arbol, &reg()),
                &resolve(area, nuevo, &reg()),
                nuevo,
                tolerado,
            )
        };
        assert!(!ok(bajo, &apilado, None), "a 8 filas uno se esconde");
        assert!(ok(alto, &apilado, None));
        assert!(!ok(alto, &juntos, Some(SlotId(2))), "uno solo a la vista");
    }

    /// Y lo que dice cuadra con lo que hace el reparto: si dice que sí, los
    /// dos huecos se colocan; si dice que no, el `Split` colapsa.
    #[test]
    fn has_room_to_split_cuadra_con_el_colapso() {
        let k = KindId::browser();
        for (w, h, dir) in [
            (40, 30, Dir::Horizontal),
            (30, 30, Dir::Horizontal),
            (30, 30, Dir::Vertical),
            (30, 9, Dir::Vertical),
        ] {
            let area = r(0, 0, w, h);
            let arbol = dos(browser(1), browser(2), dir);
            let colocados = resolve(area, &arbol, &reg()).placements.len();
            assert_eq!(
                has_room_to_split(area, dir, &k, &reg()),
                colocados == 2,
                "{w}x{h} {dir:?}: colocados {colocados}"
            );
        }
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
            sizes: vec![Size::Weight(0), Size::Weight(1)],
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

    /// Un hijo `Fixed` cobra lo suyo y los `Weight` se reparten el resto.
    #[test]
    fn un_fijo_se_lleva_lo_suyo_y_los_pesos_el_resto() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(4), Size::Weight(1)],
            children: vec![browser(1), browser(2), browser(3)],
        };
        let out = resolve(r(0, 0, 40, 24), &arbol, &reg());
        let altos: Vec<u16> = out.placements.iter().map(|(_, re)| re.height).collect();
        assert_eq!(
            altos,
            vec![10, 4, 10],
            "20 de resto entre dos pesos, y el fijo aparte"
        );
    }

    /// Un `Fixed` por debajo del mínimo de su kind SE RESPETA: el mínimo
    /// decide cuándo colapsa un reparto proporcional, no desautoriza una
    /// orden explícita.
    #[test]
    fn un_fijo_por_debajo_del_minimo_de_su_kind_se_respeta() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(1)],
            children: vec![browser(1), browser(2)],
        };
        let out = resolve(r(0, 0, 40, 24), &arbol, &reg());
        assert_eq!(out.placements.len(), 2, "no colapsa por culpa del fijo");
        assert_eq!(
            out.placements[1].1.height, 1,
            "una fila, aunque el mínimo sean 5"
        );
    }

    /// Si los fijos ya no caben se recortan en orden y los pesos se quedan sin
    /// nada. Es lo que hace que un terminal diminuto siga pintando la barra de
    /// estado en vez de la nada.
    #[test]
    fn si_los_fijos_no_caben_se_recortan_y_los_pesos_se_quedan_sin_nada() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Fixed(3), Size::Fixed(9), Size::Weight(1)],
            children: vec![browser(1), browser(2), browser(3)],
        };
        let out = resolve(r(0, 0, 40, 5), &arbol, &reg());
        let altos: Vec<u16> = out.placements.iter().map(|(_, re)| re.height).collect();
        assert_eq!(
            altos,
            vec![3, 2, 0],
            "el primero entero, el segundo recortado, el peso a cero"
        );
    }

    /// Un `Split` con un hijo fijo NO colapsa aunque el ponderado se quede sin
    /// sitio: colapsarlo se llevaría el cromo por delante. Con `orthodox` sería
    /// quedarse sin barra de estado en un terminal bajo.
    #[test]
    fn un_split_con_un_hijo_fijo_no_colapsa() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(1)],
            children: vec![browser(1), Node::slot(SlotId(2), KindId::new("status"))],
        };
        let out = resolve(r(0, 0, 40, 2), &arbol, &reg());
        assert_eq!(out.placements.len(), 2, "la barra de estado sobrevive");
        assert_eq!(out.placements[1].1.height, 1);
    }

    /// **#229**: el cromo acoplado se APARTA antes de dejar la pantalla sin un
    /// listado usable.
    ///
    /// Es el preset `full` a 40x10: sidebar fijo de 16 y columna derecha fija
    /// de 30 ya pasan de 40 columnas, así que los dos browsers cobraban CERO y
    /// desaparecían; y abajo, procesos (8) más la barra (1) dejaban la fila
    /// principal en una sola línea. Lo que quedaba en pantalla eran tres
    /// cabeceras de cromo y ni un nombre de fichero.
    #[test]
    fn el_cromo_se_aparta_antes_de_dejar_la_pantalla_sin_listado() {
        let derecha = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(6), KindId::new("viewer")),
                Node::slot(SlotId(8), KindId::new("metadata")),
            ],
        };
        let fila = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![
                Size::Fixed(16),
                Size::Weight(1),
                Size::Weight(1),
                Size::Fixed(30),
            ],
            children: vec![
                Node::slot(SlotId(5), KindId::new("places")),
                browser(1),
                browser(2),
                derecha,
            ],
        };
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(8), Size::Fixed(1)],
            children: vec![
                fila,
                Node::slot(SlotId(7), KindId::new("processes")),
                Node::slot(SlotId(4), KindId::new("status")),
            ],
        };
        let out = resolve(r(0, 0, 40, 10), &arbol, &reg());

        let listados: Vec<&(SlotId, Rect)> = out
            .placements
            .iter()
            .filter(|(id, _)| *id == SlotId(1) || *id == SlotId(2))
            .collect();
        assert!(
            listados
                .iter()
                .any(|(_, re)| re.width >= 4 && re.height >= 4),
            "ningún listado enseña una fila: {:?}",
            out.placements
        );
        // Se aparta lo GRANDE de un eje corto, y nada más: la columna derecha
        // (30 de ancho) y el panel de procesos (8 de alto). Lo que cabía se
        // queda —el sidebar y la barra de estado—, que es la diferencia entre
        // «este layout se adapta» y «este layout se rinde».
        for id in [SlotId(6), SlotId(8), SlotId(7)] {
            assert!(out.hidden.contains(&id), "{id:?} debería estar apartado");
        }
        for id in [SlotId(5), SlotId(4)] {
            assert!(
                out.placements.iter().any(|(p, _)| *p == id),
                "{id:?} cabía y se ha ido: {:?}",
                out.placements
            );
        }
        // Y apartar es SUSPENDER, con su diagnóstico: sin lo primero un panel
        // invisible se queda con su watch abierto, y sin lo segundo nadie sabe
        // por qué su sidebar no está.
        assert!(
            out.diagnostics
                .iter()
                .any(|d| matches!(d, LayoutDiagnostic::ChromeSetAside { .. })),
            "sin diagnóstico: {:?}",
            out.diagnostics
        );
    }

    /// Dos listados que fallan cada uno POR UN EJE DISTINTO siguen siendo una
    /// pantalla sin listado usable.
    ///
    /// Preguntar «¿hay alguno suficientemente ancho?» y «¿hay alguno
    /// suficientemente alto?» por separado contestaba que no falta ningún eje
    /// —uno cumple cada pregunta— y el rescate no se intentaba. Se mide sobre
    /// el MEJOR candidato, que es de quien depende que la pantalla sirva.
    #[test]
    fn dos_listados_cojos_de_ejes_distintos_no_hacen_una_pantalla_buena() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(9)],
            children: vec![
                browser(1),
                Node::Split {
                    dir: Dir::Horizontal,
                    sizes: vec![Size::Fixed(30), Size::Weight(1)],
                    children: vec![Node::slot(SlotId(3), KindId::new("viewer")), browser(2)],
                },
            ],
        };
        let out = resolve(r(0, 0, 32, 10), &arbol, &reg());
        assert!(
            out.placements
                .iter()
                .any(|(id, re)| (*id == SlotId(1) || *id == SlotId(2))
                    && re.width >= 12
                    && re.height >= 4),
            "ninguno de los dos listados quedó usable: {:?}",
            out.placements
        );
    }

    /// Apartar una pestaña de DELANTE no cambia cuál se está mirando.
    ///
    /// El índice activo es una posición, así que quitar la de delante corría a
    /// todas las de detrás: se pintaba la siguiente y se suspendía la que el
    /// usuario tenía abierta.
    #[test]
    fn apartar_una_pestana_no_cambia_la_que_se_esta_mirando() {
        let arbol = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Fixed(30), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(9), KindId::new("viewer")),
                Node::Tabs {
                    active: 1,
                    children: vec![browser(1), browser(2)],
                },
            ],
        };
        let out = resolve(r(0, 0, 34, 10), &arbol, &reg());
        assert!(
            out.placements.iter().any(|(id, _)| *id == SlotId(2)),
            "se mira la pestaña 2, y es la que tiene que quedar: {:?}",
            out.placements
        );
        assert!(out.hidden.contains(&SlotId(1)));
    }

    /// Un kind EXIGENTE no arrastra el rescate a apartar cromo persiguiendo un
    /// tamaño que la pantalla no tiene: el suelo es su mínimo TOPADO.
    #[test]
    fn un_kind_exigente_no_vacia_la_pantalla_de_cromo() {
        let mut reg = reg();
        reg.insert(crate::layout::KindDecl {
            id: KindId::new("exigente"),
            min: (80, 30),
            focusable: true,
            takes_keys: true,
            multi: false,
            roles: &[RoleId::Active],
        });
        let arbol = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![Size::Fixed(10), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(5), KindId::new("places")),
                Node::slot(SlotId(1), KindId::new("exigente")),
            ],
        };
        let out = resolve(r(0, 0, 40, 10), &arbol, &reg);
        assert!(
            out.placements.iter().any(|(id, _)| *id == SlotId(5)),
            "el sidebar cabía: 30 columnas bastan para enseñar algo"
        );
    }

    /// Un árbol con el MISMO id dos veces no pierde una copia al apartar cromo.
    ///
    /// [`super::validate`] rechaza los ids repetidos, pero `resolve` tiene que
    /// aguantar cualquier árbol, y la primera versión de esta regla apartaba
    /// «los huecos con estos ids»: se llevaba también la otra copia, que se
    /// quedaba sin pintar Y sin suspender —un watch abierto mirando a nada—. Lo
    /// cazó la propiedad de partición; el caso mínimo está pineado en
    /// `proptest-regressions/layout/resolve.txt` y esto lo dice con nombre.
    #[test]
    fn con_ids_repetidos_apartar_cromo_no_pierde_un_hueco() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![
                Node::Split {
                    dir: Dir::Vertical,
                    sizes: vec![Size::Fixed(2)],
                    children: vec![Node::slot(SlotId(10), KindId::new("tasks"))],
                },
                Node::Tabs {
                    active: 1,
                    children: vec![browser(1), Node::slot(SlotId(10), KindId::browser())],
                },
            ],
        };
        let out = resolve(r(0, 0, 4, 4), &arbol, &reg());
        let mut vistos: Vec<SlotId> = out.placements.iter().map(|(id, _)| *id).collect();
        vistos.extend(out.hidden.iter().copied());
        vistos.sort_unstable();
        let mut todos = arbol.slot_ids();
        todos.sort_unstable();
        assert_eq!(
            vistos, todos,
            "un hueco se quedó sin pintar y sin suspender"
        );
    }

    /// Cuando solo falta ALTO, el cromo de ancho no se toca. Es el mismo `full`
    /// a 80x10: sobra ancho para el sidebar y la columna derecha, y lo único
    /// que estorba son las ocho filas del panel de procesos.
    #[test]
    fn solo_se_aparta_el_cromo_del_eje_que_falta() {
        let derecha = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Weight(1)],
            children: vec![
                Node::slot(SlotId(6), KindId::new("viewer")),
                Node::slot(SlotId(8), KindId::new("metadata")),
            ],
        };
        let fila = Node::Split {
            dir: Dir::Horizontal,
            sizes: vec![
                Size::Fixed(16),
                Size::Weight(1),
                Size::Weight(1),
                Size::Fixed(30),
            ],
            children: vec![
                Node::slot(SlotId(5), KindId::new("places")),
                browser(1),
                browser(2),
                derecha,
            ],
        };
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(8), Size::Fixed(1)],
            children: vec![
                fila,
                Node::slot(SlotId(7), KindId::new("processes")),
                Node::slot(SlotId(4), KindId::new("status")),
            ],
        };
        let out = resolve(r(0, 0, 80, 10), &arbol, &reg());
        // Apartado: SOLO el panel de procesos, que es el único cromo del eje
        // que falta. El sidebar y la columna derecha son cromo de ANCHO y ahí
        // sobra sitio, así que no se tocan.
        assert!(out.hidden.contains(&SlotId(7)));
        for id in [SlotId(5), SlotId(1), SlotId(2), SlotId(6), SlotId(4)] {
            assert!(
                out.placements.iter().any(|(p, _)| *p == id),
                "{id:?} debería seguir en pantalla: {:?}",
                out.placements
            );
        }
        // Y la hoja de atributos TAMBIÉN se pinta: en las nueve filas que
        // quedan caben justas un visor (mínimo 5) y una hoja (mínimo 4). Antes
        // el reparto proporcional daba 4/5, el visor quedaba bajo su mínimo y
        // el `Split` se degradaba a pestañas escondiendo la hoja; con el suelo
        // de los pesos cada uno cobra su mínimo.
        assert!(
            out.placements.iter().any(|(p, _)| *p == SlotId(8)),
            "{:?}",
            out.placements
        );
        assert_eq!(
            out.diagnostics
                .iter()
                .filter(|d| matches!(d, LayoutDiagnostic::ChromeSetAside { .. }))
                .count(),
            1,
            "un solo panel apartado: {:?}",
            out.diagnostics
        );
    }

    /// Apartar cromo que NO arregla nada no se hace: a 40x2 ningún listado
    /// llega a su mínimo ni quitando la barra, así que la barra se queda. Es la
    /// otra mitad de #229, y la que impide que un terminal diminuto pierda el
    /// cromo a cambio de nada.
    #[test]
    fn el_cromo_no_se_aparta_si_apartarlo_no_arregla_nada() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Fixed(1)],
            children: vec![browser(1), Node::slot(SlotId(2), KindId::new("status"))],
        };
        let out = resolve(r(0, 0, 40, 2), &arbol, &reg());
        assert_eq!(out.placements.len(), 2, "la barra de estado sobrevive");
        assert!(out.hidden.is_empty());
    }

    /// Un hijo FIJO que es él mismo un listado no es cromo: apartarlo sería
    /// quitar una pantalla para dársela a otra. La tabla de
    /// [`si_los_fijos_no_caben_se_recortan_y_los_pesos_se_quedan_sin_nada`]
    /// sigue valiendo tal cual, y esto lo dice por su nombre.
    #[test]
    fn un_fijo_que_es_un_listado_no_es_cromo() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Fixed(3), Size::Fixed(9), Size::Weight(1)],
            children: vec![browser(1), browser(2), browser(3)],
        };
        let out = resolve(r(0, 0, 40, 5), &arbol, &reg());
        let altos: Vec<u16> = out.placements.iter().map(|(_, re)| re.height).collect();
        assert_eq!(altos, vec![3, 2, 0], "nada que apartar, nada que cambie");
        assert!(out.hidden.is_empty());
    }

    /// `Auto` cuenta como cero si llega hasta aquí. No debería —lo sustituye
    /// `substitute_auto`— pero un reparto no es sitio para reventar.
    #[test]
    fn un_auto_sin_sustituir_cuenta_como_cero() {
        let arbol = Node::Split {
            dir: Dir::Vertical,
            sizes: vec![Size::Weight(1), Size::Auto],
            children: vec![browser(1), browser(2)],
        };
        let out = resolve(r(0, 0, 40, 24), &arbol, &reg());
        let altos: Vec<u16> = out.placements.iter().map(|(_, re)| re.height).collect();
        assert_eq!(altos, vec![24, 0]);
    }

    // --- propiedades ---

    fn se_solapan(a: Rect, b: Rect) -> bool {
        a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
    }

    /// Tamaños arbitrarios, `Auto` incluido: `resolve` no debería verlo nunca
    /// —lo sustituye `substitute_auto`— y las propiedades tienen que aguantar
    /// que alguien se salte ese paso.
    fn tam_arbitrario() -> impl Strategy<Value = Size> {
        prop_oneof![
            (0u16..5).prop_map(Size::Weight),
            (0u16..12).prop_map(Size::Fixed),
            Just(Size::Auto),
        ]
    }

    /// Como [`arbol_arbitrario`] pero sin `Fixed` ni `Auto`.
    fn arbol_ponderado() -> impl Strategy<Value = Node> {
        let kinds = prop::sample::select(vec![
            "browser", "tasks", "viewer", "compare", "sync", "terminal",
        ]);
        let hoja = (0u32..64, kinds).prop_map(|(id, k)| Node::slot(SlotId(id), KindId::new(k)));
        hoja.prop_recursive(3, 24, 4, |inner| {
            prop_oneof![
                (prop::collection::vec(inner.clone(), 1..4), any::<bool>()).prop_map(
                    |(children, horiz)| Node::split(
                        if horiz {
                            Dir::Horizontal
                        } else {
                            Dir::Vertical
                        },
                        children
                    )
                ),
                (prop::collection::vec(inner, 1..4), 0usize..5)
                    .prop_map(|(children, active)| Node::Tabs { children, active }),
            ]
        })
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
                    prop::collection::vec(tam_arbitrario(), 1..4),
                    any::<bool>()
                )
                    .prop_map(|(children, mut sizes, horiz)| {
                        sizes.resize(children.len(), Size::Weight(1));
                        Node::Split {
                            dir: if horiz {
                                Dir::Horizontal
                            } else {
                                Dir::Vertical
                            },
                            children,
                            sizes,
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
        ///
        /// Sobre árboles SOLO PONDERADOS, porque la propiedad es del reparto
        /// proporcional: un hijo `Fixed` puede quedar por debajo de su mínimo
        /// a propósito —lo pidió el usuario— y eso tiene sus tests de tabla.
        #[test]
        fn todo_lo_colocado_cumple_su_minimo(
            arbol in arbol_ponderado(), w in 1u16..200, h in 1u16..80
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
