//! Las cinco disposiciones de fábrica.
//!
//! Viven en TOML —el MISMO formato que `layouts/<nombre>.toml` y que el cuerpo
//! de la sesión de L2 (ADR 0058)— y no en Rust, para que lo que norte trae de
//! serie y lo que un usuario guarda sean la misma cosa: se puede copiar un
//! preset, cambiarle dos números y quedárselo.
//!
//! `NAMES` y [`source`] son items SEPARADOS y se prueban uno contra otro, como
//! en [`crate::keymap::presets`]: un preset añadido a uno y no al otro rompe
//! CI en vez de desaparecer en silencio.
//!
//! Los ficheros no se escriben a mano. El test de este módulo construye los
//! cinco árboles con los constructores de [`Node`] y compara; con
//! `NORTE_UPDATE_GOLDEN=1` los reescribe. Anidar cuatro niveles de TOML a mano
//! es como un `sizes` acaba con una entrada menos que sus `children`.

use super::{LayoutError, Node};

/// La de siempre: dos listados, la franja de tareas y la barra de estado.
pub const ORTHODOX: &str = include_str!("../../presets/layout/orthodox.toml");
/// Un solo listado. Un terminal estrecho, una sesión ssh, una pantalla
/// compartida en una llamada.
pub const SIMPLE: &str = include_str!("../../presets/layout/simple.toml");
/// Dos listados con el sidebar de sitios.
pub const KRUSADER: &str = include_str!("../../presets/layout/krusader.toml");
/// Un listado con sitios, visor acoplado y el panel de procesos.
pub const EXPLORER: &str = include_str!("../../presets/layout/explorer.toml");
/// Todo encendido: sitios, dos listados, visor, atributos y procesos.
pub const FULL: &str = include_str!("../../presets/layout/full.toml");

/// Los nombres de las cinco, en el orden en que las enseña el selector.
pub const NAMES: &[&str] = &["orthodox", "simple", "krusader", "explorer", "full"];

/// El TOML de un preset de fábrica, o `None` si ese nombre no es uno.
#[must_use]
pub fn source(name: &str) -> Option<&'static str> {
    match name {
        "orthodox" => Some(ORTHODOX),
        "simple" => Some(SIMPLE),
        "krusader" => Some(KRUSADER),
        "explorer" => Some(EXPLORER),
        "full" => Some(FULL),
        _ => None,
    }
}

/// El árbol de un preset de fábrica, parseado y validado.
///
/// # Errors
///
/// [`LayoutError::NotFound`] si el nombre no es de fábrica, y lo que devuelvan
/// el parseo o [`super::validate`] — que en la práctica no ocurre, porque los
/// tests de este módulo parsean los cinco en cada CI.
///
/// ```
/// use norte_frontend::layout::presets;
///
/// let arbol = presets::tree("simple").expect("de fábrica");
/// // Un listado, la franja de tareas y la barra de estado.
/// assert_eq!(arbol.slot_ids().len(), 3);
/// assert!(presets::tree("no-existe").is_err());
/// ```
pub fn tree(name: &str) -> Result<Node, LayoutError> {
    let texto = source(name).ok_or_else(|| LayoutError::NotFound(name.to_owned()))?;
    let arbol: Node = toml::from_str(texto).map_err(|e| LayoutError::Parse(e.to_string()))?;
    super::validate(&arbol)?;
    Ok(arbol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::config::to_toml;
    use crate::layout::{Bindings, Dir, Edge, Follow, KindId, KindRegistry, RoleId, Size, SlotId};

    /// Los huecos bien conocidos. Los cuatro primeros son los que el TUI ya
    /// tiene nombrados desde L1a, así que cambiar de preset no renumera los
    /// paneles por debajo.
    const LEFT: SlotId = SlotId(1);
    const RIGHT: SlotId = SlotId(2);
    const TASKS: SlotId = SlotId(3);
    const STATUS: SlotId = SlotId(4);
    const PLACES: SlotId = SlotId(5);
    const PREVIEW: SlotId = SlotId(6);
    const PROCESSES: SlotId = SlotId(7);
    const METADATA: SlotId = SlotId(8);

    fn browser(id: SlotId) -> Node {
        Node::slot(id, KindId::browser())
    }

    /// Un panel que MIRA al listado activo: el visor acoplado y la hoja de
    /// atributos. Sin el vínculo son cajas vacías.
    fn siguiendo(id: SlotId, kind: &str) -> Node {
        Node::slot_bound(
            id,
            KindId::new(kind),
            Bindings {
                follows: Some(Follow::Role(RoleId::Active)),
            },
        )
    }

    /// El cuerpo con lo de abajo y la barra de estado. Los cinco terminan
    /// igual: cuerpo ponderado, franja o panel, y una fila de estado.
    fn con_cromo(cuerpo: Node, abajo: Node, alto_abajo: Size) -> Node {
        Node::Split {
            dir: Dir::Vertical,
            children: vec![cuerpo, abajo, Node::slot(STATUS, KindId::new("status"))],
            sizes: vec![Size::Weight(1), alto_abajo, Size::Fixed(1)],
        }
    }

    fn tasks() -> Node {
        Node::slot(TASKS, KindId::new("tasks"))
    }

    fn processes() -> Node {
        Node::slot(PROCESSES, KindId::new("processes"))
    }

    fn esperado(name: &str) -> Node {
        match name {
            "orthodox" => con_cromo(
                Node::split(Dir::Horizontal, vec![browser(LEFT), browser(RIGHT)]),
                tasks(),
                Size::Auto,
            ),
            "simple" => con_cromo(browser(LEFT), tasks(), Size::Auto),
            // El sidebar va al lado de los LISTADOS, no al lado del cromo: es
            // exactamente lo que produce `dock`, y el último test lo fija.
            "krusader" => con_cromo(
                Node::Split {
                    dir: Dir::Horizontal,
                    children: vec![
                        Node::slot(PLACES, KindId::new("places")),
                        browser(LEFT),
                        browser(RIGHT),
                    ],
                    sizes: vec![Size::Fixed(16), Size::Weight(1), Size::Weight(1)],
                },
                tasks(),
                Size::Auto,
            ),
            "explorer" => con_cromo(
                Node::Split {
                    dir: Dir::Horizontal,
                    children: vec![
                        Node::slot(PLACES, KindId::new("places")),
                        browser(LEFT),
                        siguiendo(PREVIEW, "viewer"),
                    ],
                    sizes: vec![Size::Fixed(16), Size::Weight(1), Size::Weight(1)],
                },
                processes(),
                Size::Fixed(8),
            ),
            "full" => con_cromo(
                Node::Split {
                    dir: Dir::Horizontal,
                    children: vec![
                        Node::slot(PLACES, KindId::new("places")),
                        browser(LEFT),
                        browser(RIGHT),
                        Node::split(
                            Dir::Vertical,
                            vec![
                                siguiendo(PREVIEW, "viewer"),
                                siguiendo(METADATA, "metadata"),
                            ],
                        ),
                    ],
                    sizes: vec![
                        Size::Fixed(16),
                        Size::Weight(1),
                        Size::Weight(1),
                        Size::Fixed(30),
                    ],
                },
                processes(),
                Size::Fixed(8),
            ),
            otro => panic!("preset desconocido: {otro}"),
        }
    }

    fn ruta(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("presets/layout")
            .join(format!("{name}.toml"))
    }

    /// El fichero que se envía ES el árbol de arriba. Con `NORTE_UPDATE_GOLDEN`
    /// se reescribe; sin él, se compara.
    #[test]
    fn los_cinco_ficheros_son_los_cinco_arboles() {
        for name in NAMES {
            let quiero = to_toml(&esperado(name)).expect("serializa");
            if std::env::var_os("NORTE_UPDATE_GOLDEN").is_some() {
                std::fs::write(ruta(name), &quiero).expect("escribe");
            }
            let hay = std::fs::read_to_string(ruta(name))
                .expect("el preset — regenéralo con NORTE_UPDATE_GOLDEN=1");
            assert_eq!(hay, quiero, "{name}: regenéralo con NORTE_UPDATE_GOLDEN=1");
        }
    }

    /// Lo que de verdad importa: lo que `tree` devuelve es lo que se esperaba.
    #[test]
    fn los_cinco_parsean_a_lo_que_dicen_ser() {
        for name in NAMES {
            assert_eq!(tree(name).expect(name), esperado(name), "{name}");
        }
    }

    /// Un kind que este binario no declara se pinta como una caja con su
    /// nombre. En un preset DE FÁBRICA eso sería un preset roto de serie.
    #[test]
    fn ningun_preset_nombra_un_kind_que_no_existe() {
        let reg = KindRegistry::builtin();
        for name in NAMES {
            let arbol = tree(name).expect(name);
            for id in arbol.slot_ids() {
                let kind = arbol.kind_of(id).expect("kind");
                assert!(
                    reg.get(kind).is_some(),
                    "{name}: el kind {} no existe",
                    kind.as_str()
                );
            }
        }
    }

    /// Ids repetidos: `validate` ya los rechaza, así que esto comprueba que
    /// ninguno de los cinco llega a producir el error.
    #[test]
    fn ningun_preset_repite_un_hueco() {
        for name in NAMES {
            assert!(
                tree(name).expect(name).duplicate_slot_ids().is_empty(),
                "{name}"
            );
        }
    }

    /// TODO preset, en TODA pantalla razonable, deja un listado que se puede
    /// usar.
    ///
    /// `full` se envió con una pantalla de 40×10 sin ningún listado —los fijos
    /// cobran primero, 16 del sidebar más 30 de la columna derecha sobre 40
    /// columnas dejaban los dos browsers a cero— y el snapshot que la
    /// aprobó era la única puerta que había: pintaba lo que pintaba, así que
    /// bendijo el vacío (#244 M4). Lo que faltaba era la PROPIEDAD, y es
    /// esto. El remedio (#229, apartar el cromo) vive en `resolve`; este test
    /// es lo que dice si sigue haciendo su trabajo.
    #[test]
    fn ningun_preset_deja_una_pantalla_sin_listado_usable() {
        use crate::layout::{Rect, resolve};

        // El suelo que el rescate de #229 promete: `resolve::CONTENIDO`, el
        // tope con el que se acota el mínimo de cada kind. En pantallas
        // holgadas se exige además el mínimo PROPIO del listado, que es lo
        // que se ve cuando no hay que apretar nada.
        const USABLE: (u16, u16) = (12, 4);

        let reg = KindRegistry::builtin();
        let (mw, mh) = reg.min_of(&crate::layout::KindId::browser());
        for name in NAMES {
            let arbol = tree(name).expect(name);
            for (w, h) in [(40_u16, 10_u16), (60, 15), (80, 24), (120, 40)] {
                // A 120 columnas no hay nada que apretar y se exige el
                // mínimo PROPIO del listado; por debajo manda el suelo del
                // rescate, que es lo que #229 promete — `full` a 80 deja 17
                // columnas por listado (16 de sidebar + 30 de hoja de
                // atributos son fijos) y eso es apretado, no roto.
                let (pw, ph) = if w >= 120 { (mw, mh) } else { USABLE };
                let res = resolve(Rect::new(0, 0, w, h), &arbol, &reg);
                let mejor = res
                    .placements
                    .iter()
                    .filter(|(id, _)| {
                        arbol
                            .kind_of(*id)
                            .is_some_and(|k| *k == crate::layout::KindId::browser())
                    })
                    .map(|(_, r)| (r.width, r.height))
                    .max();
                let Some((bw, bh)) = mejor else {
                    panic!("{name} a {w}x{h}: ningún listado colocado");
                };
                assert!(
                    bw >= pw && bh >= ph,
                    "{name} a {w}x{h}: el mejor listado mide {bw}x{bh}, por debajo de {pw}x{ph}"
                );
            }
        }
    }

    /// `NAMES` y `source` son dos items y se pueden desincronizar. No aquí.
    #[test]
    fn el_catalogo_y_la_busqueda_dicen_lo_mismo() {
        for name in NAMES {
            assert!(source(name).is_some(), "{name} en NAMES y no en source");
        }
        assert!(source("no-existe").is_none());
        assert!(matches!(tree("no-existe"), Err(LayoutError::NotFound(_))));
    }

    /// `krusader` es `orthodox` con el sidebar acoplado. Si esto se rompe, o
    /// el preset dejó de ser alcanzable con el teclado, o `dock` cambió de
    /// opinión sobre dónde va un sidebar; las dos cosas hay que mirarlas.
    #[test]
    fn krusader_es_orthodox_con_el_sidebar_puesto() {
        let acoplado = tree("orthodox").expect("orthodox").dock(
            LEFT,
            Edge::Left,
            Size::Fixed(16),
            &Node::slot(PLACES, KindId::new("places")),
        );
        assert_eq!(acoplado, tree("krusader").expect("krusader"));
    }
}
