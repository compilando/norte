# Layout presets, phase A — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship five factory layouts you can pick at runtime, the two panel
kinds two of them need (`processes`, `metadata`), and the fix that makes a
fixed-width sidebar resizable (#227).

**Architecture:** The five layouts are TOML files embedded in `norte-frontend`,
in the same format `layouts/<name>.toml` already uses, exposed by a
`layout::presets` module that mirrors `keymap::presets` (a `NAMES` list and a
`source()` lookup, tested against each other). `panel::orthodox()` becomes a
lookup into it. The two new kinds follow L3's `places`/`preview` pattern
exactly: a state type in `TuiPanel`, a `want()` decision function that is pure
over the tree and the roles, a renderer keyed off `placed_of_kind`, and a
three-state toggle command. The picker is a `norte-frontend` state type with a
TUI renderer, like `ColumnsPicker`.

**Tech stack:** Rust, `serde`/`toml`, ratatui, nextest, Fluent (`norte-i18n`),
insta-style snapshot goldens under `crates/norte-cli/tests/goldens/`.

**Spec:** `docs/superpowers/specs/2026-08-18-layout-presets-and-ui-session-design.md`
(phase A only; phase B, the L2 session, gets its own plan once this lands).

---

## Before you start

Read the spec's phase A section. Then read these three files, in this order —
they are the pattern this plan copies, and nothing here will make sense without
them:

- `crates/norte-frontend/src/keymap/presets.rs` — the `NAMES`/`source()` pair
  and why they are tested against each other.
- `crates/norte-tui/src/preview.rs` — a following panel: `KIND`, `slot()`,
  `want()`, and why a hidden slot produces no target.
- `crates/norte-tui/src/app.rs:3101` (`toggle_places`) — the three-state
  toggle: open, take the keyboard, close.

## The gate

`just t norte-frontend` and `just t norte-tui` in the RED→GREEN loop, `just c`
when you touch lint surface. **ONE** `just ci-fast` after task 6. **ONE**
`just ci` before the merge. Never re-run the gate to find out whether a fix
worked — reproduce the single failure with `just t <crate>`.

`just t` does not run doctests. Task 3 and task 8 add documented public items:
`cargo test -p norte-frontend --doc` after each, and
`cargo doc -p norte-frontend --no-deps` if you write a `[`link`]`.

## File structure

| file | responsibility |
| --- | --- |
| `crates/norte-frontend/src/layout/tree.rs` | **modify** — `resize` moves a `Fixed` child (#227) |
| `crates/norte-frontend/src/layout/kinds.rs` | **modify** — declare `processes` and `metadata` |
| `crates/norte-frontend/presets/layout/*.toml` | **create** — the five trees, five files |
| `crates/norte-frontend/src/layout/presets.rs` | **create** — `NAMES`, `source`, `tree` |
| `crates/norte-frontend/src/layout/mod.rs` | **modify** — `pub mod presets;` |
| `crates/norte-frontend/src/layout_picker.rs` | **create** — picker state and its ASCII preview |
| `crates/norte-tui/src/panel.rs` | **modify** — `orthodox()` delegates; two `TuiPanel` variants |
| `crates/norte-tui/src/metadata.rs` | **create** — what the metadata panel should show |
| `crates/norte-tui/src/processes.rs` | **create** — the processes panel's own cursor |
| `crates/norte-tui/src/ui.rs` | **modify** — three renderers |
| `crates/norte-tui/src/app.rs` | **modify** — three toggles, picker state, apply-a-preset |
| `crates/norte-tui/src/main.rs` | **modify** — command dispatch, dialog keys, `--layout` |
| `crates/norte-frontend/src/keymap/catalogue.rs` | **modify** — three command ids |
| `crates/norte-frontend/src/menu.rs` | **modify** — three menu items |
| `crates/norte-i18n/i18n/{en,es}.ftl` | **modify** — every user-visible string |
| `crates/norte-help/topics/{en,es}/panes.md` | **modify** — document the three commands |
| `crates/norte-help/tests/corpus.rs` | **modify** — `DOCUMENTED` grows by three |
| `crates/norte-cli/tests/goldens/help-en.json` | **regenerate** — `NORTE_UPDATE_GOLDEN=1` |
| `CHANGELOG.md` | **modify** — what a user gets |

---

### Task 1: `layout.grow` moves a fixed sidebar (#227)

`Node::resize` only touches `Size::Weight` children. Three of the five presets
have a `Fixed` sidebar, so without this they ship unresizable. A `Fixed` child
moves in **cells**, two per press; a `Weight` child keeps today's behaviour
exactly.

**Files:**
- Modify: `crates/norte-frontend/src/layout/tree.rs:544` (`resize`)
- Test: same file, `mod tests`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/norte-frontend/src/layout/tree.rs`:

```rust
/// #227: un hijo FIJO —el ancho del sidebar— se mueve en CELDAS. Antes
/// `resize` solo tocaba pesos, así que el panel de sitios no se podía
/// ensanchar con el teclado y los presets con sidebar nacían atascados.
#[test]
fn un_hijo_fijo_se_mueve_en_celdas() {
    let arbol = Node::Split {
        dir: Dir::Horizontal,
        children: vec![
            Node::slot(SlotId(5), KindId::new("places")),
            Node::slot(SlotId(1), KindId::browser()),
        ],
        sizes: vec![Size::Fixed(16), Size::Weight(1)],
    };
    let ancho = |n: &Node| match n {
        Node::Split { sizes, .. } => sizes[0],
        _ => panic!("split"),
    };
    assert_eq!(ancho(&arbol.resize(SlotId(5), 1)), Size::Fixed(18));
    assert_eq!(ancho(&arbol.resize(SlotId(5), -1)), Size::Fixed(14));
}

/// El tope de abajo existe para que no se pueda dejar en cero: un panel de
/// ancho cero no se ve y no hay forma de volver a agrandarlo.
#[test]
fn un_hijo_fijo_no_baja_de_dos_ni_pasa_de_cien() {
    let arbol = |n: u16| Node::Split {
        dir: Dir::Horizontal,
        children: vec![
            Node::slot(SlotId(5), KindId::new("places")),
            Node::slot(SlotId(1), KindId::browser()),
        ],
        sizes: vec![Size::Fixed(n), Size::Weight(1)],
    };
    let ancho = |n: &Node| match n {
        Node::Split { sizes, .. } => sizes[0],
        _ => panic!("split"),
    };
    assert_eq!(ancho(&arbol(2).resize(SlotId(5), -1)), Size::Fixed(2));
    assert_eq!(ancho(&arbol(100).resize(SlotId(5), 1)), Size::Fixed(100));
}

/// Y un hijo PONDERADO sigue haciendo exactamente lo de antes.
#[test]
fn un_hijo_ponderado_no_cambia_de_comportamiento() {
    let arbol = Node::Split {
        dir: Dir::Horizontal,
        children: vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(2), KindId::browser()),
        ],
        sizes: vec![Size::Weight(1), Size::Weight(1)],
    };
    let peso = |n: &Node| match n {
        Node::Split { sizes, .. } => sizes[0],
        _ => panic!("split"),
    };
    assert_eq!(peso(&arbol.resize(SlotId(1), 1)), Size::Weight(2));
    assert_eq!(peso(&arbol.resize(SlotId(1), -1)), Size::Weight(1));
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `just t norte-frontend`
Expected: `un_hijo_fijo_se_mueve_en_celdas` FAILS with
`assertion \`left == right\` failed: left: Fixed(16), right: Fixed(18)`. The
weight test PASSES already — that is the point of having it.

- [ ] **Step 3: Implement**

Replace the body of `resize` in `crates/norte-frontend/src/layout/tree.rs`:

```rust
    /// Cambia el tamaño del hijo que contiene `id` en `delta`.
    ///
    /// Un hijo PONDERADO se mueve de peso, entre 1 y 10. Un hijo FIJO se mueve
    /// en CELDAS, dos por pulsación, entre 2 y 100: un sidebar pidió un ancho
    /// concreto, y hasta #227 eso quería decir que el teclado no podía
    /// cambiarlo — que es un ancho impuesto, no un ancho elegido. El tope de
    /// abajo no es cosmético: a cero el panel desaparece y con él la forma de
    /// devolverlo.
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
```

`Size::Auto` stays untouched on purpose: it is substituted before the split is
computed, so a number stored here would be overwritten and the key would look
broken.

- [ ] **Step 4: Run the tests**

Run: `just t norte-frontend`
Expected: PASS, and no other layout test moves.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-frontend/src/layout/tree.rs
git commit -m "fix(layout): a fixed sidebar moves in cells, not in nothing (#227)"
```

---

### Task 2: declare the `processes` and `metadata` kinds

**Files:**
- Modify: `crates/norte-frontend/src/layout/kinds.rs:62-79` (the `builtin()` list)
- Test: same file, `mod tests`

- [ ] **Step 1: Write the failing test**

```rust
/// Los dos kinds nuevos de la fase A. Ninguno opta a un rol: un panel de
/// procesos y una hoja de atributos jamás son el destino de una copia, y
/// dejarles el rol `Target` es como una tecla de copiar acaba apuntando a
/// una caja que no es un directorio.
#[test]
fn processes_y_metadata_se_enfocan_pero_no_son_destino() {
    let reg = KindRegistry::builtin();
    for id in ["processes", "metadata"] {
        let d = reg.get(&KindId::new(id)).expect("declarado");
        assert!(d.focusable, "{id} se enfoca");
        assert!(d.takes_keys, "{id} toma teclas");
        assert!(!d.multi, "{id} es uno solo");
        assert!(d.roles.is_empty(), "{id} no opta a rol");
        assert!(!reg.holds_role(&KindId::new(id), RoleId::Target));
    }
    assert_eq!(reg.min_of(&KindId::new("processes")), (30, 4));
    assert_eq!(reg.min_of(&KindId::new("metadata")), (24, 4));
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `just t norte-frontend`
Expected: FAIL, `declarado` — `get` returns `None`.

- [ ] **Step 3: Implement**

In `builtin()`, after the `sync` line:

```rust
                // El panel de procesos (fase A): la franja `tasks` sigue
                // existiendo y sigue siendo lo que trae `orthodox`. Este es el
                // panel de verdad — se enfoca, se recorre y cancela — y hay
                // uno. Mínimo 30x4: una fila lleva nombre, barra y porcentaje.
                decl("processes", (30, 4), true, true, false, SIN_ROLES),
                // La hoja de atributos (fase A): sigue al rol `active` con el
                // mismo vínculo que el visor acoplado. 24 columnas es lo que
                // ocupa la etiqueta más larga con su valor al lado.
                decl("metadata", (24, 4), true, true, false, SIN_ROLES),
```

- [ ] **Step 4: Run the tests**

Run: `just t norte-frontend`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-frontend/src/layout/kinds.rs
git commit -m "feat(layout): two kinds a screen can hold, processes and metadata"
```

---

### Task 3: the five presets

The trees are **defined in Rust in the test** and **stored as TOML on disk**.
The test builds each tree with the constructors, serialises it, and compares it
against the shipped file; with `NORTE_UPDATE_GOLDEN=1` it writes the file
instead. That way the file is the artefact norte reads and a user can copy —
and nobody hand-nests a four-level tree in TOML, which is how those get one
`sizes` entry out of step with its `children`.

**Files:**
- Create: `crates/norte-frontend/src/layout/presets.rs`
- Create: `crates/norte-frontend/presets/layout/{orthodox,simple,krusader,explorer,full}.toml`
- Modify: `crates/norte-frontend/src/layout/mod.rs`
- Test: inside `presets.rs`

- [ ] **Step 1: Write the module, with the trees and the failing tests**

Create `crates/norte-frontend/src/layout/presets.rs`:

```rust
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
/// tests de este módulo prueban los cinco en cada CI.
///
/// ```
/// use norte_frontend::layout::presets;
/// let arbol = presets::tree("simple").expect("de fábrica");
/// assert_eq!(arbol.slot_ids().len(), 3);
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
    use crate::layout::{
        Bindings, Dir, Follow, KindId, KindRegistry, RoleId, Size, SlotId, config::to_toml,
    };

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

    /// El cuerpo (lo que sea que vaya arriba) con la franja o el panel de
    /// abajo y la barra de estado. Los cinco presets terminan igual.
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
            // El sidebar va al lado de los LISTADOS, no al lado del cromo:
            // es exactamente lo que produce `dock`, y el test de abajo lo fija.
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

    /// El fichero que se envía ES el árbol de arriba. Con `NORTE_UPDATE_GOLDEN`
    /// se reescribe; sin él, se compara. Nadie anida a mano cuatro niveles de
    /// TOML, y por eso el árbol se escribe con los constructores.
    #[test]
    fn los_cinco_ficheros_son_los_cinco_arboles() {
        for name in NAMES {
            let quiero = to_toml(&esperado(name)).expect("serializa");
            let ruta = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("presets/layout")
                .join(format!("{name}.toml"));
            if std::env::var_os("NORTE_UPDATE_GOLDEN").is_some() {
                std::fs::write(&ruta, &quiero).expect("escribe");
            }
            let hay = std::fs::read_to_string(&ruta)
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
                assert!(reg.get(kind).is_some(), "{name}: kind {} no existe", kind.as_str());
            }
        }
    }

    /// Ids repetidos: `validate` ya lo rechaza, así que esto comprueba que
    /// ninguno de los cinco llega a producir el error.
    #[test]
    fn ningun_preset_repite_un_hueco() {
        for name in NAMES {
            assert!(tree(name).expect(name).duplicate_slot_ids().is_empty(), "{name}");
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
            crate::layout::Edge::Left,
            Size::Fixed(16),
            &Node::slot(PLACES, KindId::new("places")),
        );
        assert_eq!(acoplado, tree("krusader").expect("krusader"));
    }
}
```

- [ ] **Step 2: Declare the module**

In `crates/norte-frontend/src/layout/mod.rs`, beside the other `pub mod` lines:

```rust
pub mod presets;
```

- [ ] **Step 3: Create the five files empty and generate them**

```bash
mkdir -p crates/norte-frontend/presets/layout
for n in orthodox simple krusader explorer full; do
  : > crates/norte-frontend/presets/layout/$n.toml
done
NORTE_UPDATE_GOLDEN=1 just t norte-frontend
```

Expected: the run writes five files and passes. **Read the generated TOML** —
it is what a user will copy, and this is the only moment anybody looks at it.

- [ ] **Step 4: Run the tests clean**

Run: `just t norte-frontend`
Expected: PASS, including `krusader_es_orthodox_con_el_sidebar_puesto`. If that
one fails, `dock` puts the sidebar somewhere else than the tree above says —
fix the tree, not the test, and say so in the commit.

Run: `cargo test -p norte-frontend --doc`
Expected: PASS — the doctest on `tree` counts, and `just t` never runs it.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-frontend/src/layout/presets.rs \
        crates/norte-frontend/src/layout/mod.rs \
        crates/norte-frontend/presets/layout/
git commit -m "feat(layout): five factory layouts, in the format a user can copy"
```

---

### Task 4: `panel::orthodox()` becomes a lookup

**The equality test goes in before the function comes out.** A
snapshot-anchored refactor whose anchor moves in the same commit as the thing
it anchors has proved nothing.

**Files:**
- Modify: `crates/norte-tui/src/panel.rs:455-471`
- Test: same file, `mod tests`

- [ ] **Step 1: Write the pinning test**

```rust
/// El preset de fábrica y la función que el TUI ha tenido siempre son el
/// mismo árbol. Va ANTES de borrar la función: es lo único que dice que el
/// fichero TOML no cambió la pantalla de nadie.
#[test]
fn el_orthodox_de_fabrica_es_el_de_siempre() {
    assert_eq!(
        norte_frontend::layout::presets::tree("orthodox").expect("de fábrica"),
        orthodox()
    );
}
```

- [ ] **Step 2: Run it**

Run: `just t norte-tui`
Expected: PASS immediately. If it fails, task 3's tree is wrong — stop and fix
that, because everything downstream is measured against this screen.

- [ ] **Step 3: Make the function a lookup**

Replace the body of `orthodox()` in `crates/norte-tui/src/panel.rs` (keep the
doc comment and its diagram, they are still true):

```rust
#[must_use]
pub fn orthodox() -> Node {
    // Del preset de fábrica, no de un árbol escrito aquí: dos definiciones de
    // la misma pantalla se separan, y la que se cargue de un fichero ganaría
    // sin que nadie lo note. El test de abajo fija que son iguales.
    norte_frontend::layout::presets::tree("orthodox")
        .unwrap_or_else(|e| unreachable!("el preset de fábrica no parsea: {e}"))
}
```

The `unreachable!` is justified by the tests in task 3, which parse all five on
every CI run — that is the invariant hard rule 6 asks you to state.

- [ ] **Step 4: Run the tests**

Run: `just t norte-tui`
Expected: PASS, and **the `orthodox` screen snapshots do not move**. If any
snapshot changes, revert the step: the tree differs.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-tui/src/panel.rs
git commit -m "refactor(tui): one definition of the orthodox screen, not two"
```

---

### Task 5: the `metadata` panel

A near-twin of `preview`, and deliberately so. The difference that matters: it
reads **nothing**. The `Entry` under the cursor is already in the listing the
browser holds — path, kind, size, mtime and the provider attributes the pane
requested — so the panel is a pure function of state the frontend already has.

**Files:**
- Create: `crates/norte-tui/src/metadata.rs`
- Modify: `crates/norte-tui/src/lib.rs` (declare the module)
- Modify: `crates/norte-tui/src/panel.rs` (a `TuiPanel` variant and its accessors)
- Test: `crates/norte-tui/src/metadata.rs`, `mod tests`

- [ ] **Step 1: Write the module and its failing tests**

Create `crates/norte-tui/src/metadata.rs`:

```rust
//! La hoja de atributos (fase A): qué debería estar enseñando.
//!
//! Como [`crate::preview`], aquí vive la DECISIÓN y solo la decisión, por la
//! misma razón: es una función pura del árbol, los roles y el cursor, así que
//! las reglas se fijan con tests y no con prosa.
//!
//! A diferencia del visor acoplado, este panel **no lee nada**: la `Entry` que
//! enseña ya está en el listado. Un panel que sigue al cursor y además pide
//! datos por cada fila es como se convierte bajar por un directorio en una
//! tormenta de peticiones.

use norte_frontend::layout::{Resolved, SlotId};
use norte_proto::Entry;

use crate::app::App;

/// El kind que ocupa un hueco de atributos.
pub const KIND: &str = "metadata";

/// Qué debería estar enseñando la hoja.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    /// Esta entrada, que el listado ya tiene.
    Entry(Box<Entry>),
    /// Nada que enseñar, y esta clave Fluent dice por qué.
    Note(&'static str),
}

/// El hueco de atributos COLOCADO en este reparto, si lo hay.
#[must_use]
pub fn slot(app: &App, res: &Resolved) -> Option<SlotId> {
    res.placements
        .iter()
        .map(|(id, _)| *id)
        .find(|id| app.layout.kind_of(*id).is_some_and(|k| k.as_str() == KIND))
}

/// Qué toca enseñar, y en qué hueco. `None` si no hay hueco colocado.
#[must_use]
pub fn want(app: &App, res: &Resolved) -> Option<(SlotId, Want)> {
    let hueco = slot(app, res)?;
    let mut diags = Vec::new();
    let seguido =
        norte_frontend::layout::resolve_follow(&app.layout, hueco, &app.roles, &mut diags)
            .or_else(|| app.roles.get(norte_frontend::layout::RoleId::Active))?;
    let pane = app.panes.browser(seguido)?;
    match pane.selected() {
        Some(e) => Some((hueco, Want::Entry(Box::new(e.clone())))),
        None => Some((hueco, Want::Note("metadata-empty"))),
    }
}
```

Tests, in the same file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Un hueco que el reparto no colocó —cerrado, detrás de una pestaña, o
    /// colapsado por falta de sitio— no produce objetivo. La suspensión no es
    /// una comprobación aparte que alguien pueda olvidarse de escribir.
    #[test]
    fn un_hueco_que_no_se_ve_no_pide_nada() {
        let app = crate::app::test_support::app_con_metadata_oculto();
        let res = crate::app::test_support::reparto(&app, 80, 24);
        assert!(want(&app, &res).is_none());
    }

    /// Un listado vacío no deja la hoja con lo de antes puesto: dice que no
    /// hay nada bajo el cursor.
    #[test]
    fn un_listado_sin_cursor_dice_que_no_hay_nada() {
        let app = crate::app::test_support::app_con_metadata_y_listado(&[]);
        let res = crate::app::test_support::reparto(&app, 80, 24);
        assert!(matches!(
            want(&app, &res),
            Some((_, Want::Note("metadata-empty")))
        ));
    }

    /// Y con cursor, la entrada que el listado YA tiene: cero peticiones.
    #[test]
    fn con_cursor_ensena_la_entrada_que_ya_estaba() {
        let app = crate::app::test_support::app_con_metadata_y_listado(&["uno.txt"]);
        let res = crate::app::test_support::reparto(&app, 80, 24);
        let Some((_, Want::Entry(e))) = want(&app, &res) else {
            panic!("una entrada");
        };
        assert!(e.path.as_bytes().ends_with(b"uno.txt"));
    }
}
```

The three `test_support` helpers may not exist yet. Look in
`crates/norte-tui/src/app.rs` for how the L3 preview tests build an `App` and a
`Resolved` (`grep -n "mod test_support" crates/norte-tui/src`); reuse them, and
if the metadata variants are missing, add them **beside** the preview ones,
built the same way — `app_con_preview_oculto` is the model.

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-tui`
Expected: FAIL to compile — `crate::metadata` is not declared, and `TuiPanel`
has no metadata variant yet.

- [ ] **Step 3: Declare the module and the panel state**

In `crates/norte-tui/src/lib.rs`, beside `pub mod preview;`:

```rust
pub mod metadata;
```

In `crates/norte-tui/src/panel.rs`, add a variant to `TuiPanel`:

```rust
    /// La hoja de atributos (fase A): qué se está enseñando ahora mismo.
    ///
    /// Guarda la entrada pintada, no la ruta: la hoja se dibuja entera desde
    /// ella y no hay una segunda lectura que pueda llegar tarde.
    Metadata(Box<Option<norte_proto::Entry>>),
```

Extend every `match` in that file that enumerates the variants — the compiler
lists them; `as_browser`, `as_browser_mut`, `as_places`, `as_places_mut` all
return `None` for it. Add the accessor pair:

```rust
    /// La hoja, si este panel es una.
    #[must_use]
    pub fn as_metadata(&self) -> Option<&Option<norte_proto::Entry>> {
        match self {
            Self::Metadata(e) => Some(e),
            _ => None,
        }
    }

    /// La hoja, para mutarla.
    pub fn as_metadata_mut(&mut self) -> Option<&mut Option<norte_proto::Entry>> {
        match self {
            Self::Metadata(e) => Some(e),
            _ => None,
        }
    }
```

Do **not** convert the existing exhaustive matches to `_ =>` while you are
there: they are exhaustive on purpose, so that the next kind cannot be added
without visiting them.

And the `PaneSlots` pair the renderer will call, beside `places`/`places_mut`
(`panel.rs:263`) and `insert_places` (`panel.rs:295`):

```rust
    /// La hoja de atributos de un hueco.
    #[must_use]
    pub fn metadata(&self, id: SlotId) -> Option<&Option<norte_proto::Entry>> {
        self.store.get(id).and_then(TuiPanel::as_metadata)
    }

    /// La hoja, para mutarla.
    pub fn metadata_mut(&mut self, id: SlotId) -> Option<&mut Option<norte_proto::Entry>> {
        self.store.get_mut(id).and_then(TuiPanel::as_metadata_mut)
    }

    /// Mete una hoja de atributos en un hueco recién abierto.
    pub fn insert_metadata(&mut self, id: SlotId, e: Option<norte_proto::Entry>) {
        self.store.insert(id, TuiPanel::Metadata(Box::new(e)));
    }
```

- [ ] **Step 4: Run the tests**

Run: `just t norte-tui`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-tui/src/metadata.rs crates/norte-tui/src/lib.rs \
        crates/norte-tui/src/panel.rs
git commit -m "feat(tui): what an attribute sheet should show, and when it shows nothing"
```

---

### Task 6: the `processes` panel

The `TaskBoard` already exists and is already drawn as a strip. The panel adds
a cursor and cancelling; the rows come from `app.board.rows()`, so there is no
second copy of the task list to keep in step.

**Files:**
- Create: `crates/norte-tui/src/processes.rs`
- Modify: `crates/norte-tui/src/lib.rs`, `crates/norte-tui/src/panel.rs`
- Test: `crates/norte-tui/src/processes.rs`, `mod tests`

- [ ] **Step 1: Write the module and its failing tests**

Create `crates/norte-tui/src/processes.rs`:

```rust
//! El panel de procesos (fase A): el cursor, y nada más.
//!
//! Las filas son las del `TaskBoard` que ya pinta la franja: este panel no
//! guarda una segunda copia, porque dos listas de tareas se separan y la que
//! se ve deja de ser la que se cancela.
//!
//! **No hay pausa.** El protocolo tiene `task.cancel` y no tiene otra cosa, y
//! un control que no hace lo que dice es peor que un control que falta.

/// El kind que ocupa un hueco de procesos.
pub const KIND: &str = "processes";

/// El estado propio del panel: dónde está el cursor.
#[derive(Debug, Default)]
pub struct Processes {
    cursor: usize,
}

impl Processes {
    /// Dónde está el cursor, acotado a `filas`.
    ///
    /// Se acota al LEER y no al mover: las filas aparecen y desaparecen solas
    /// —una tarea termina y se barre—, así que un cursor guardado siempre
    /// puede haberse quedado fuera.
    #[must_use]
    pub fn cursor(&self, filas: usize) -> usize {
        self.cursor.min(filas.saturating_sub(1))
    }

    /// Sube.
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Baja, sin pasarse de la última fila.
    pub fn down(&mut self, filas: usize) {
        self.cursor = (self.cursor + 1).min(filas.saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_cursor_no_se_sale_por_abajo() {
        let mut p = Processes::default();
        p.down(2);
        p.down(2);
        p.down(2);
        assert_eq!(p.cursor(2), 1);
    }

    #[test]
    fn el_cursor_no_se_sale_por_arriba() {
        let mut p = Processes::default();
        p.up();
        assert_eq!(p.cursor(3), 0);
    }

    /// Una tarea termina y su fila se va: el cursor guardado apuntaba a una
    /// fila que ya no está, y lo que se lee es la última que sí está.
    #[test]
    fn un_cursor_de_una_fila_que_ya_no_esta_se_acota_al_leer() {
        let mut p = Processes::default();
        p.down(5);
        p.down(5);
        assert_eq!(p.cursor(5), 2);
        assert_eq!(p.cursor(1), 0);
    }

    /// Sin filas no hay cursor que valga, y `saturating_sub` es lo que evita
    /// el pánico de índice en un panel abierto con el sistema en reposo.
    #[test]
    fn sin_filas_el_cursor_es_cero() {
        let p = Processes::default();
        assert_eq!(p.cursor(0), 0);
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-tui`
Expected: FAIL to compile — the module is not declared.

- [ ] **Step 3: Declare it and add the panel variant**

In `crates/norte-tui/src/lib.rs`:

```rust
pub mod processes;
```

In `crates/norte-tui/src/panel.rs`, add to `TuiPanel`:

```rust
    /// El panel de procesos (fase A): su cursor. Las filas son del
    /// `TaskBoard`, que es de `App`.
    Processes(Box<crate::processes::Processes>),
```

Extend the exhaustive matches, and add `as_processes` / `as_processes_mut` on
`TuiPanel` plus the `PaneSlots` trio, all following the shape task 5 used:

```rust
    /// El panel de procesos de un hueco.
    #[must_use]
    pub fn processes(&self, id: SlotId) -> Option<&crate::processes::Processes> {
        self.store.get(id).and_then(TuiPanel::as_processes)
    }

    /// El panel, para mover su cursor.
    pub fn processes_mut(&mut self, id: SlotId) -> Option<&mut crate::processes::Processes> {
        self.store.get_mut(id).and_then(TuiPanel::as_processes_mut)
    }

    /// Mete un panel de procesos en un hueco recién abierto.
    pub fn insert_processes(&mut self, id: SlotId, p: crate::processes::Processes) {
        self.store.insert(id, TuiPanel::Processes(Box::new(p)));
    }
```

- [ ] **Step 4: Run the tests**

Run: `just t norte-tui`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-tui/src/processes.rs crates/norte-tui/src/lib.rs \
        crates/norte-tui/src/panel.rs
git commit -m "feat(tui): a processes panel with a cursor, and no button that lies"
```

- [ ] **Step 6: Spend the first gate run**

Run: `just ci-fast`
Expected: green. This is **one** of the two runs this plan budgets. If it is
red, reproduce the single failure with `just t <crate>` and fix it there — do
not re-run `ci-fast` to find out.

---

### Task 7: draw them, and toggle them

**Files:**
- Modify: `crates/norte-tui/src/ui.rs` (two renderers, beside `draw_places`)
- Modify: `crates/norte-tui/src/app.rs` (two toggles, beside `toggle_places`)
- Modify: `crates/norte-tui/src/main.rs` (command dispatch)
- Test: `crates/norte-tui/tests/layout_anchor.rs`

- [ ] **Step 1: Write the failing test**

In `crates/norte-tui/tests/layout_anchor.rs`:

```rust
/// El panel de procesos se abre, toma el teclado y se cierra: el mismo ciclo
/// de tres estados que el sidebar. Y el de atributos igual.
#[test]
fn los_dos_paneles_nuevos_ciclan_como_el_sidebar() {
    let mut app = crate::helpers::app_basica();
    assert!(app.processes_slot().is_none());
    app.toggle_processes();
    let id = app.processes_slot().expect("abierto");
    assert_eq!(app.key_owner(), norte_tui::app::KeyOwner::Processes);
    app.toggle_processes();
    assert_eq!(app.key_owner(), norte_tui::app::KeyOwner::Panes);
    assert_eq!(app.processes_slot(), Some(id), "sigue abierto");
    app.toggle_processes();
    // Con el teclado FUERA, la tercera pulsación lo vuelve a tomar; para
    // cerrar hay que estar dentro. Es el ciclo del sidebar, no el del visor.
    assert_eq!(app.key_owner(), norte_tui::app::KeyOwner::Processes);
    app.toggle_processes();
    assert!(app.processes_slot().is_none(), "cerrado");
}
```

Match the helper names already in that file — read it first; `helpers::` may be
called something else.

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-tui`
Expected: FAIL to compile — no `toggle_processes`, no `KeyOwner::Processes`.

- [ ] **Step 3: Implement the toggles**

In `crates/norte-tui/src/app.rs`, add `Processes` and `Metadata` to the
`KeyOwner` enum, then, modelled exactly on `toggle_places` (`app.rs:3101`):

```rust
    /// Abre el panel de procesos, le da el teclado, o lo cierra.
    ///
    /// Tres estados como el sidebar y NO como el visor acoplado: un panel de
    /// procesos se abre para mirar y para cancelar algo concreto, así que
    /// tomar el teclado al abrir es lo que se espera. (L3 aprendió la
    /// distinción por las malas: el preview hace lo contrario porque se abre
    /// para seguir navegando.)
    pub fn toggle_processes(&mut self) {
        use norte_frontend::layout::{Edge, KindId, Node, Size};
        match self.processes_slot() {
            Some(id) if self.key_owner == KeyOwner::Processes => {
                if let Some(nuevo) = self.layout.close_slot(id) {
                    self.layout = nuevo;
                    self.panes.refresh_visible(&self.layout);
                    self.history.retain_tree(&self.layout);
                }
                self.key_owner = KeyOwner::Panes;
            }
            Some(_) => self.key_owner = KeyOwner::Processes,
            None => {
                let id = self.mint_slot();
                self.panes
                    .insert_processes(id, crate::processes::Processes::default());
                self.layout = self.layout.dock(
                    self.focused_slot(),
                    Edge::Bottom,
                    Size::Fixed(8),
                    &Node::slot(id, KindId::new(crate::processes::KIND)),
                );
                self.panes.refresh_visible(&self.layout);
                self.key_owner = KeyOwner::Processes;
            }
        }
    }
```

`toggle_metadata` is the same function with `Edge::Right`, `Size::Fixed(30)`,
`crate::metadata::KIND`, `insert_metadata(id, None)` and `KeyOwner::Metadata`.
Add `processes_slot()` and `metadata_slot()` beside `places_slot()`, and
`insert_processes` / `insert_metadata` beside `insert_places` in `panel.rs`.

Dispatch in `crates/norte-tui/src/main.rs`, beside `"layout.places"`:

```rust
        "layout.processes" => app.toggle_processes(),
        "layout.metadata" => app.toggle_metadata(),
```

- [ ] **Step 4: Implement the renderers**

In `crates/norte-tui/src/ui.rs`, after the `places` block (around line 521):

```rust
    if let Some((id, rect)) = placed_of_kind(&res, &app.layout, crate::processes::KIND)
        && let Some(p) = app.panes.processes(id)
    {
        draw_processes(
            frame,
            rect,
            p,
            &app.board,
            app.key_owner() == crate::app::KeyOwner::Processes,
            &app.theme,
        );
    }
    if let Some((id, rect)) = placed_of_kind(&res, &app.layout, crate::metadata::KIND)
        && let Some(e) = app.panes.metadata(id)
    {
        draw_metadata(
            frame,
            rect,
            e.as_ref(),
            app.key_owner() == crate::app::KeyOwner::Metadata,
            app,
        );
    }
```

Write `draw_processes` and `draw_metadata` beside `draw_places`. For the
progress bar reuse the percentage computation already in `draw_tasks`
(`ui.rs:2601`) — **move it into a small helper and call it from both**, rather
than copying it: two copies of a percentage is how one of them ends up dividing
by a total that can be zero.

Sizes in the metadata sheet go through `human_bytes_short`, which rounds down
and never truncates from the head. L3's lesson, and it is one line to get
wrong: `38.2 GiB` clipped from the left paints `8.2 GiB`, which is a **false
number**, not a clipped label.

- [ ] **Step 5: Feed the panels each frame**

Wherever `before_frame` reconciles the preview (search `preview::want` in
`main.rs`), do the same for metadata:

```rust
    if let Some((hueco, quiere)) = crate::metadata::want(app, &res) {
        let destino = match quiere {
            crate::metadata::Want::Entry(e) => Some(*e),
            crate::metadata::Want::Note(_) => None,
        };
        if let Some(slot) = app.panes.metadata_mut(hueco) {
            *slot = destino;
        }
    }
```

`processes` needs nothing here: it reads `app.board` at paint time.

- [ ] **Step 6: Run the tests**

Run: `just t norte-tui`
Expected: PASS, and the existing `orthodox` snapshots unchanged — neither panel
is in `orthodox`.

- [ ] **Step 7: Drive it in tmux before believing it**

```bash
just link
tmux new-session -d -s norte 'ntc'
tmux send-keys -t norte ':' # open the palette, run layout.processes, then layout.metadata
tmux capture-pane -t norte -p | head -40
```

L3 found two real bugs this way on a suite of 800 green tests — a panel that
switched itself off, and a key that stopped working while its own panel had the
keyboard. **The screen that dispatches is the one that matters**, so check both
panels with the keyboard inside them and outside them. Kill the session when
done: `tmux kill-session -t norte`.

- [ ] **Step 8: Commit**

```bash
git add crates/norte-tui/src/ui.rs crates/norte-tui/src/app.rs \
        crates/norte-tui/src/main.rs crates/norte-tui/src/panel.rs \
        crates/norte-tui/tests/layout_anchor.rs
git commit -m "feat(tui): the processes panel and the attribute sheet on screen"
```

---

### Task 8: the picker

**Files:**
- Create: `crates/norte-frontend/src/layout_picker.rs`
- Modify: `crates/norte-frontend/src/lib.rs`
- Modify: `crates/norte-tui/src/app.rs`, `ui.rs`, `main.rs`
- Test: inside `layout_picker.rs`

- [ ] **Step 1: Write the state and its failing tests**

Create `crates/norte-frontend/src/layout_picker.rs`:

```rust
//! El selector de disposiciones: qué filas hay y qué se ve de cada una.
//!
//! Vive aquí y no en el TUI por la regla 7 —la lógica no va en un frontend— y
//! porque la GUI necesitará el mismo selector con otro pintor.

use crate::layout::{Node, Resolved, presets};

/// Una fila del selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// El nombre con el que se carga.
    pub name: String,
    /// De fábrica, o de `layouts/<nombre>.toml`.
    pub factory: bool,
    /// Si el nombre coincide con un preset de KEYMAP. El selector avisa,
    /// porque elegir esta disposición no cambia ni una tecla.
    pub shares_keymap_name: bool,
}

/// El selector.
#[derive(Debug)]
pub struct LayoutPicker {
    rows: Vec<Row>,
    cursor: usize,
}

impl LayoutPicker {
    /// Abre el selector con los cinco de fábrica y los nombres de usuario que
    /// se le pasen (los ficheros los lista quien tiene el disco delante).
    #[must_use]
    pub fn open(user: &[String]) -> Self {
        let fila = |name: &str, factory: bool| Row {
            name: name.to_owned(),
            factory,
            shares_keymap_name: crate::keymap::presets::NAMES.contains(&name),
        };
        let mut rows: Vec<Row> = presets::NAMES.iter().map(|n| fila(n, true)).collect();
        // Un fichero de usuario que se llama como uno de fábrica NO se
        // duplica: gana el de usuario, que es lo que hace la config en todas
        // las demás capas.
        for n in user {
            if let Some(r) = rows.iter_mut().find(|r| &r.name == n) {
                r.factory = false;
            } else {
                rows.push(fila(n, false));
            }
        }
        Self { rows, cursor: 0 }
    }

    /// Las filas, en orden.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Dónde está el cursor.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor.min(self.rows.len().saturating_sub(1))
    }

    /// Sube.
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Baja.
    pub fn down(&mut self) {
        self.cursor = (self.cursor + 1).min(self.rows.len().saturating_sub(1));
    }

    /// El nombre elegido.
    #[must_use]
    pub fn chosen(&self) -> Option<&str> {
        self.rows.get(self.cursor()).map(|r| r.name.as_str())
    }
}

/// La previsualización: cajas dibujadas a partir del REPARTO de `arbol` en un
/// área de `w`x`h`, una línea por fila de celdas.
///
/// Del reparto y no de un dibujo guardado al lado del fichero: un dibujo
/// guardado empieza a mentir en cuanto alguien toca los tamaños, y el usuario
/// no tiene forma de saber cuál de los dos es la pantalla de verdad.
#[must_use]
pub fn preview(arbol: &Node, w: u16, h: u16, decls: &crate::layout::KindRegistry) -> Vec<String> {
    let res: Resolved = crate::layout::resolve(
        crate::layout::Rect { x: 0, y: 0, width: w, height: h },
        arbol,
        decls,
    );
    let mut lienzo = vec![vec![' '; w as usize]; h as usize];
    for (id, r) in &res.placements {
        let inicial = arbol
            .kind_of(*id)
            .and_then(|k| k.as_str().chars().next())
            .unwrap_or('?');
        for y in r.y..r.y.saturating_add(r.height).min(h) {
            for x in r.x..r.x.saturating_add(r.width).min(w) {
                let borde = y == r.y
                    || y + 1 == r.y + r.height
                    || x == r.x
                    || x + 1 == r.x + r.width;
                lienzo[y as usize][x as usize] = if borde { '·' } else { inicial };
            }
        }
    }
    lienzo.into_iter().map(|f| f.into_iter().collect()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::KindRegistry;

    #[test]
    fn los_cinco_de_fabrica_salen_en_orden() {
        let p = LayoutPicker::open(&[]);
        let nombres: Vec<&str> = p.rows().iter().map(|r| r.name.as_str()).collect();
        assert_eq!(nombres, presets::NAMES);
        assert!(p.rows().iter().all(|r| r.factory));
    }

    /// `krusader` es también un preset de KEYMAP, y elegir la disposición no
    /// cambia ni una tecla. La fila lo dice; si esto se rompe, el aviso
    /// desaparece y la coincidencia de nombres pasa a ser una trampa.
    #[test]
    fn la_fila_avisa_cuando_el_nombre_es_tambien_de_keymap() {
        let p = LayoutPicker::open(&[]);
        let f = |n: &str| p.rows().iter().find(|r| r.name == n).expect(n).shares_keymap_name;
        assert!(f("krusader"));
        assert!(f("orthodox"));
        assert!(!f("explorer"));
    }

    /// Un fichero del usuario que se llama como uno de fábrica no aparece dos
    /// veces: gana el del usuario, como en cualquier otra capa de config.
    #[test]
    fn un_layout_de_usuario_con_nombre_de_fabrica_no_se_duplica() {
        let p = LayoutPicker::open(&["simple".to_owned(), "mio".to_owned()]);
        assert_eq!(p.rows().len(), presets::NAMES.len() + 1);
        assert!(!p.rows().iter().find(|r| r.name == "simple").expect("simple").factory);
    }

    #[test]
    fn el_cursor_no_se_sale() {
        let mut p = LayoutPicker::open(&[]);
        for _ in 0..20 {
            p.down();
        }
        assert_eq!(p.chosen(), Some("full"));
        for _ in 0..20 {
            p.up();
        }
        assert_eq!(p.chosen(), Some("orthodox"));
    }

    /// La previsualización sale del reparto: `simple` tiene UN listado y
    /// `orthodox` dos, así que la primera fila de celdas se ve distinta.
    #[test]
    fn la_vista_previa_distingue_una_disposicion_de_otra() {
        let reg = KindRegistry::builtin();
        let uno = preview(&presets::tree("simple").expect("s"), 20, 8, &reg);
        let dos = preview(&presets::tree("orthodox").expect("o"), 20, 8, &reg);
        assert_ne!(uno, dos);
        assert_eq!(uno.len(), 8);
        assert!(uno.iter().all(|l| l.chars().count() == 20));
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `just t norte-frontend`
Expected: FAIL to compile — the module is not declared.

- [ ] **Step 3: Declare it**

In `crates/norte-frontend/src/lib.rs`, beside `pub mod columns_picker;`:

```rust
pub mod layout_picker;
```

- [ ] **Step 4: Run the tests**

Run: `just t norte-frontend`
Expected: PASS.

Run: `cargo doc -p norte-frontend --no-deps`
Expected: no warnings. `just c` does not check intra-doc links and `just t`
does not either; this module has several.

- [ ] **Step 5: Wire it into the TUI**

Modelled on `columns_picker` — read `app.rs:3396` (`open_columns_picker`),
`ui.rs:1379` (`draw_columns_picker`) and `main.rs:4823` (its dialog keys), and
do the same three things:

```rust
// app.rs
    /// El selector de disposiciones (fase A), como el de columnas: el estado
    /// es de norte-frontend, aquí solo vive el `Option`.
    pub layout_picker: Option<norte_frontend::layout_picker::LayoutPicker>,

    /// Abre el selector con los cinco de fábrica y los ficheros del usuario.
    pub fn open_layout_picker(&mut self, user: &[String]) {
        self.layout_picker = Some(norte_frontend::layout_picker::LayoutPicker::open(user));
    }

    /// Aplica una disposición por nombre: primero de fábrica, luego fichero.
    ///
    /// Un nombre que no carga NO deja a norte sin pantalla: se avisa y se deja
    /// la que había, que es la política de config del repositorio para una
    /// recarga en caliente.
    pub fn apply_layout(&mut self, name: &str, dir: &std::path::Path) {
        let cargado = norte_frontend::layout::presets::tree(name)
            .or_else(|_| norte_frontend::layout::config::load(dir, name));
        match cargado {
            Ok(arbol) => {
                self.set_layout(arbol);
                self.message = Some(norte_i18n::ta("msg-layout-applied", &[("name", name)]));
            }
            Err(e) => {
                self.message = Some(norte_i18n::ta(
                    "msg-layout-load-failed",
                    &[("name", name), ("err", &e.to_string())],
                ));
            }
        }
    }
```

`norte_i18n::ta` takes `&[(&str, &str)]` — the same call `main.rs:1920` already
makes for `msg-layout-load-failed`, which is where a user meets this message
today.

`dir` is the configuration directory that `main.rs:1914` already passes to
`layout::config::load`; hold it in `App` when the config is read rather than
recomputing it at the keypress, and pass it in from the dispatch site.

Dialog keys in `main.rs`, beside the columns picker's:

```rust
        "dialog.cancel" => app.layout_picker = None,
        "dialog.confirm" => {
            let elegido = app
                .layout_picker
                .as_ref()
                .and_then(|p| p.chosen())
                .map(str::to_owned);
            let dir = app.config_dir.clone(); // el mismo que usa `main.rs:1914`
            if let Some(name) = elegido {
                app.apply_layout(&name, &dir);
            }
            app.layout_picker = None;
        }
        "dialog.up" => { if let Some(p) = app.layout_picker.as_mut() { p.up(); } }
        "dialog.down" => { if let Some(p) = app.layout_picker.as_mut() { p.down(); } }
```

Use the dialog command ids the columns picker actually uses — copy them from
`main.rs:4823` rather than the names above if they differ. Command dispatch:

```rust
        "layout.pick" => app.open_layout_picker(&layouts_de_usuario),
```

`layouts_de_usuario` is the file stems of `<config>/layouts/*.toml`, read with
`std::fs::read_dir` at the moment the picker opens. That is a blocking read on
a directory with five files, in a frontend, at a keypress — the same thing
`config::load` already does one line away. Do not route it through the core.

Render in `ui.rs` beside `draw_columns_picker`, using
`layout_picker::preview(...)` for the box on the right and
`t("layout-picker-keymap-note")` under a row whose `shares_keymap_name` is set.

- [ ] **Step 6: `--layout` on the command line**

In `crates/norte-tui/src/main.rs`:

```rust
const VALUE_FLAGS: &[&str] = &["--preset", "--socket", "--cd-file", "--layout"];
```

Read it with `args.text("--layout")` beside `--preset`, let it win over
`[ui] layout`, and add the line to `USAGE`:

```text
      --layout <NAME>  Start with this layout (orthodox, simple, krusader,
                       explorer, full, or one of yours)
```

- [ ] **Step 7: Run the tests, then drive it**

Run: `just t norte-tui`
Expected: PASS.

```bash
just link && ntc --layout full
```

Expected: sidebar, two panels, viewer and attribute sheet on the right,
processes across the bottom. Then open the picker and switch to `simple` and
back. A layout switch must not renumber the panels underneath — the directory
you were in stays where it was.

- [ ] **Step 8: Commit**

```bash
git add crates/norte-frontend/src/layout_picker.rs crates/norte-frontend/src/lib.rs \
        crates/norte-tui/src/app.rs crates/norte-tui/src/ui.rs crates/norte-tui/src/main.rs
git commit -m "feat(tui,frontend): pick a layout, with a preview drawn from the tree"
```

---

### Task 9: pin the five screens

Two things the unit tests cannot see: what each preset actually paints, and
what a one-panel layout does to an operation that needs two.

**Files:**
- Test: `crates/norte-tui/tests/layout_anchor.rs`

- [ ] **Step 1: Write the snapshot test**

```rust
/// Cada preset, pintado, a dos tamaños. El grande es la pantalla de verdad;
/// el pequeño es donde tres columnas ya no caben, así que es el que ejercita
/// el colapso — el camino que ningún test de `resolve` a 80x24 toca.
#[test]
fn los_cinco_presets_pintan_lo_que_dicen() {
    for name in norte_frontend::layout::presets::NAMES {
        for (w, h) in [(80_u16, 24_u16), (40, 10)] {
            let mut app = crate::helpers::app_basica();
            app.set_layout(
                norte_frontend::layout::presets::tree(name).expect("de fábrica"),
            );
            let pantalla = crate::helpers::pintar(&app, w, h);
            insta::assert_snapshot!(format!("preset-{name}-{w}x{h}"), pantalla);
        }
    }
}
```

Use whatever the file already uses to render into a string — read the existing
snapshot tests in `crates/norte-tui/tests/` and copy their harness; if they use
a hand-rolled `TestBackend` dump rather than `insta`, use that instead. Do not
introduce a second snapshot mechanism.

- [ ] **Step 2: Write the one-panel test**

```rust
/// Con UN listado no hay «el otro pane», así que una copia no tiene destino
/// por defecto. La regla de L1 es que la operación PREGUNTA — abre el diálogo
/// de ruta — en vez de fallar. `simple` es el primer preset donde eso pasa de
/// ser hipotético, y este test es lo que impide que vuelva a ser un error.
#[test]
fn con_un_solo_listado_una_copia_pregunta_el_destino() {
    let mut app = crate::helpers::app_basica();
    app.set_layout(norte_frontend::layout::presets::tree("simple").expect("s"));
    assert_eq!(app.panes.len(), 1);
    app.start_copy(); // el mismo camino que F5; usa el nombre que tenga
    assert!(app.modal.is_some(), "pregunta la ruta en vez de fallar");
}
```

Find the real entry point for `F5` (`grep -n '"fs.copy"\|Command::Copy' crates/norte-tui/src/main.rs`)
and call that, not an invented `start_copy`. If the path dialog is not what
happens today with one pane, **stop**: that is a bug the spec predicted, and it
gets its own failing test and fix before this task continues.

- [ ] **Step 3: Run them**

Run: `just t norte-tui`
Expected: the snapshots are created on the first run — **read all ten** before
accepting them. `orthodox` at 80×24 must equal the snapshot that already
exists; if a preset paints an empty box where a panel should be, its tree is
wrong, not the snapshot.

- [ ] **Step 4: Commit**

```bash
git add crates/norte-tui/tests/
git commit -m "test(tui): what each of the five screens paints, and what one panel does to a copy"
```

---

### Task 10: the surfaces a command has to appear on

Three new commands means the catalogue, the menu, both locales, the help topic,
the corpus test and the CLI golden. L3 measured this: **one command moves four
goldens**, and the golden diff is where you read the sentence a user will
actually see.

**Files:**
- Modify: `crates/norte-frontend/src/keymap/catalogue.rs:126`
- Modify: `crates/norte-frontend/src/menu.rs:89`
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl`
- Modify: `crates/norte-help/topics/en/panes.md`, `crates/norte-help/topics/es/panes.md`
- Modify: `crates/norte-help/tests/corpus.rs:480`
- Regenerate: `crates/norte-cli/tests/goldens/help-en.json`

- [ ] **Step 1: Run the corpus test to see it fail**

First add the three ids to the catalogue, after `layout.preview`:

```rust
    // Fase A: el selector de disposiciones y los dos paneles nuevos.
    live("layout.pick", false),
    live("layout.processes", false),
    live("layout.metadata", false),
```

The `false` is `counts` — whether the command takes a numeric prefix — not
whether it is bound. **None of the three gets a chord in any preset.** Fifteen
`layout.*` commands times seven presets is what #228 is about, and binding
three of them here would leave the family half-bound with no rule for which
half. They ship reachable from the palette and the menu bar, greyed in the
reference sheet, which is the pattern #132–#140 already established.

Run: `just t norte-help`
Expected: FAIL — `DOCUMENTED` has 113 entries and the catalogue now has three
live commands that no topic documents.

- [ ] **Step 2: Document them**

`crates/norte-help/tests/corpus.rs`: `const DOCUMENTED: [&str; 116]`, with
`"layout.metadata"`, `"layout.pick"` and `"layout.processes"` inserted in the
existing sort order.

`crates/norte-help/topics/en/panes.md`, at the end of the layout section:

```markdown
## Choosing a layout

`layout.pick` opens the list of layouts: the five norte ships with —
`orthodox`, `simple`, `krusader`, `explorer` and `full` — and any of your own
under `layouts/` in the configuration directory. Each row shows what the screen
would look like. `ntc --layout <name>` starts in one directly, and
`[ui] layout` in `norte.toml` makes it the one you always get.

A layout named after a file manager does not change your keys. `krusader` the
layout and `krusader` the keymap preset are two separate settings, and the list
says so where the names meet.

`layout.processes` opens the processes panel: one row per running task, with
what it is doing and how far along it is, and cancel on the row under the
cursor. The task strip at the foot of the screen does not go away — the panel
is what you open when you want to act on a task rather than watch it.

`layout.metadata` opens the attribute sheet, which shows what is known about
the entry under the cursor and follows it as you move.
```

The Spanish topic says the same thing in Spanish. Keep both files in step —
the corpus test checks that every documented command appears in every locale.

- [ ] **Step 3: Strings**

`en.ftl`, beside the existing `layout` keys:

```text
menu-item-layout-pick = Choose layout…
menu-item-layout-processes = Processes panel
menu-item-layout-metadata = Attribute sheet
help-cmd-layout-pick = choose a layout
help-cmd-layout-processes = show or hide the processes panel
help-cmd-layout-metadata = show or hide the attribute sheet
layout-picker-title = Layout
layout-picker-factory = built in
layout-picker-keymap-note = this is a layout, not a keymap: your keys do not change
msg-layout-applied = layout "{$name}"
metadata-empty = nothing under the cursor
processes-empty = nothing running
```

`es.ftl` gets the same keys in Spanish. Add the three menu items to
`crates/norte-frontend/src/menu.rs` in the layout group, beside
`"layout.places"`.

- [ ] **Step 4: Regenerate the CLI golden and read the diff**

```bash
NORTE_UPDATE_GOLDEN=1 cargo nextest run -p norte-cli --no-fail-fast
git diff crates/norte-cli/tests/goldens/help-en.json
```

Expected: three commands appear, each with the sentence from `help-cmd-*`.
**Read it.** A help sentence that reads badly here reads badly to every user,
and this is the only place it is visible as prose.

- [ ] **Step 5: Run the tests**

Run: `just t norte-help && just t norte-frontend && just t norte-cli`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-frontend/src/keymap/catalogue.rs crates/norte-frontend/src/menu.rs \
        crates/norte-i18n/i18n/en.ftl crates/norte-i18n/i18n/es.ftl \
        crates/norte-help/topics/en/panes.md crates/norte-help/topics/es/panes.md \
        crates/norte-help/tests/corpus.rs crates/norte-cli/tests/goldens/help-en.json
git commit -m "feat(help,i18n): the three new commands, on every surface that lists one"
```

---

### Task 11: close the branch

- [ ] **Step 1: Changelog**

Add to `CHANGELOG.md` under `## [Unreleased]` / `### Added`, in the register the
rest of that file uses — what a user gets, not what was refactored:

```markdown
- **Five screens to choose from, instead of one.** `orthodox` is what norte has
  always looked like and still the default. `simple` is one panel, for a narrow
  terminal or a shared screen. `krusader` adds the places sidebar to the two.
  `explorer` is one panel with the sidebar, the docked viewer and a processes
  panel. `full` turns everything on. Pick one from the layout list, start in one
  with `ntc --layout <name>`, or set `[ui] layout` and always get it. A layout
  named after a file manager does not touch your keys: the layout and the keymap
  preset are separate settings, and the list says so where the names meet.

- **A processes panel, and an attribute sheet.** The processes panel gives every
  running task a row with its progress and cancels the one under the cursor —
  the strip at the foot of the screen stays exactly as it was, and the panel is
  what you open when you want to act on a task rather than watch it. The
  attribute sheet shows what is known about the entry under the cursor and
  follows it as you move, reading nothing to do it: everything it shows was
  already in the listing.
```

And under `### Fixed`:

```markdown
- **A sidebar you can widen.** Grow and shrink did nothing to a panel with a
  fixed width, which is every sidebar, so the places panel was stuck at the
  width it opened with. It now moves two columns at a time (#227).
```

- [ ] **Step 2: The one full gate run**

Run: `just ci`
Expected: green. Run the recipes one at a time in the foreground if it is
killed as a background job — `lint`, `test`, `docs`, `gui-ci`, `cov` — and
never through `| tail`.

- [ ] **Step 3: Review before merging, not after**

Dispatch, in parallel, with the commit range and what the change is for:

- `rust-reviewer` — the whole diff.
- `encoding-auditor` — the metadata sheet renders filenames and sizes. Ask it
  specifically about the size formatting (a truncated size is a false number)
  and about the `Entry` path reaching the screen without a lossy conversion.

No `protocol-guardian`: phase A touches no protocol. No `security-reviewer`:
no journal, no policy, no plugin surface. Apply BLOCKER and MAJOR; say which
MINORs you skipped and why.

- [ ] **Step 4: Close #227 and record the work**

```bash
gh issue close 227 --comment "Fixed: a fixed child now moves in cells, two per press, clamped to [2,100]. Shipped with the five factory layouts, three of which have a fixed sidebar."
```

Then update `MEMORY.md` and its layout entry with what phase A landed, and note
that phase B (L2, the session) is next and has a spec but no plan yet.

---

## Notes for whoever executes this

**Nothing will notify you.** No monitor exists. Do not `sleep`, do not
`timeout N tail -f /dev/null`, do not wait for a signal — run the next step.

**Two agents never share this tree.** If work is split, each gets its own
worktree (`scripts/wt.sh <name>`).

**Read `git diff --cached --stat` before every commit.** An empty commit whose
message claims to close an issue has happened here before.
