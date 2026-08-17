# L1a — the layout engine, with the screen unchanged

> **For agentic workers:** REQUIRED SUB-SKILL: use `superpowers:subagent-driven-development`
> (recommended) or `superpowers:executing-plans` to implement this plan
> task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** land the pure layout engine in `norte-frontend` and make the TUI paint
from it, with the `orthodox` screen byte-identical before and after.

**Architecture:** a tree of `Split`/`Tabs`/`Slot` nodes resolves against a `Rect`
into `Resolved { placements, hidden, focus_order }`. The painter, the mouse hit
test and the pagination window all read that one value instead of each
recomputing the split. Panel state moves out of named fields (`App.panes[2]`)
into a `SlotStore<P>` keyed by `SlotId`.

**Tech stack:** Rust, `norte-frontend` (no UI dependencies at all), ratatui in
the TUI only, `insta` snapshots, `proptest`, nextest via `just`.

**Design:** `docs/superpowers/specs/2026-08-17-layout-slots-tabs-design.md`
**Decision:** `docs/adr/0058-a-screen-is-a-tree-the-core-keeps-and-does-not-read.md`

---

## What this plan is not

L1 in the design has six steps. This plan is steps 1–4: **the engine goes in and
nothing visible changes.** Tabs, the fifteen new commands, roles bound to keys,
the layout config file and the GUI are **L1b**, a separate plan, listed at the
bottom so nobody implements half of it here.

Two deliberate omissions, so they are not mistaken for gaps:

- **No `[ui.layout]` config.** With exactly one possible layout the setting does
  nothing. It arrives in L1b with the layouts that make it meaningful.
- **`Tabs` is defined in the types and resolved by the engine, but nothing
  builds one.** The node type must exist now because `resolve` and `focus_order`
  are written against it and retrofitting hidden subtrees later would rewrite
  both. No command creates one until L1b.

## Gate budget for this plan

Per `CLAUDE.md`. The RED→GREEN loop is `just t norte-frontend` (~78 s) and
`just t norte-tui`, unlimited. **`just ci-fast` exactly twice**: after Task 6 and
after Task 8. **`just ci` once**, at the close. Do not re-run the gate to see
whether a fix worked — reproduce the single failure in the crate suite.

`norte-frontend` enables `#![warn(missing_docs)]`, so every public item needs
rustdoc. Doc links are not checked by `just c` or `just t`: after any task that
adds a `[`Type`]` link, run `cargo doc -p norte-frontend --no-deps` once.

## File structure

| file | responsibility |
| --- | --- |
| `crates/norte-frontend/src/layout/mod.rs` | module docs, re-exports, `LayoutError` / `LayoutDiagnostic` |
| `crates/norte-frontend/src/layout/tree.rs` | `Rect`, `SlotId`, `KindId`, `Dir`, `Node`, `Params`, `Bindings`, `Follow`, `RoleId`; serde |
| `crates/norte-frontend/src/layout/kinds.rs` | `KindDecl`, `KindRegistry`, the built-in declarations |
| `crates/norte-frontend/src/layout/resolve.rs` | `Resolved`, `resolve` — splits, weights, minimums, collapse, hidden |
| `crates/norte-frontend/src/layout/focus.rs` | `focus_next` / `focus_prev` over a `Resolved` |
| `crates/norte-frontend/src/layout/roles.rs` | `Roles`, reconciliation, `resolve_follow` |
| `crates/norte-frontend/src/layout/store.rs` | `SlotStore<P>`, orphan retention |
| `crates/norte-tui/src/panel.rs` | `TuiPanel`, the TUI's concrete panel state, and the fixed slot ids |
| `crates/norte-tui/src/ui.rs` | painting from `Resolved`; `pane_geometry`/`pane_list_rows` retire |
| `crates/norte-tui/tests/layout_anchor.rs` | the one test that says what the engine believes it painted is on screen |

---

### Task 1: Anchor the screen before touching anything

The single most valuable test in this plan. It must exist and pass **before** any
production code moves, because it is the only thing that can tell you the
refactor shifted a cell.

**Files:**
- Create: `crates/norte-tui/tests/layout_anchor.rs`

- [ ] **Step 1: Read how the existing render tests build an `App` and a buffer**

Read `crates/norte-tui/tests/render.rs` and `crates/norte-tui/tests/mouse.rs`.
They already construct an `App` with a fixed listing and render into a
`ratatui::backend::TestBackend` of a known size. Reuse that helper verbatim — do
not invent a second way to build a test `App`.

- [ ] **Step 2: Write the anchoring test**

The claim is: *for every pane, the rows the geometry says are list rows contain
list content in the painted buffer, and the rows outside it do not.* Today that
is checked against `pane_geometry`; after Task 9 the same test reads
`Resolved`, and it is the same assertion either way.

```rust
//! El ancla del refactor de layout: lo que el motor CREE que pintó es lo que
//! hay en el buffer. Hoy lo dice `ui::pane_geometry`; tras L1a lo dice
//! `Resolved`. La aserción no cambia — cambia de quién la lee, y por eso este
//! test es el que detecta que el reparto se movió una celda.

#[test]
fn la_geometria_declarada_coincide_con_las_filas_pintadas() {
    let (mut app, area) = app_de_prueba_con(60 /* entradas */, 100, 30);
    let buf = pintar(&mut app, area);
    let geom = norte_tui::ui::pane_geometry(&app, area).expect("dos panes pintados");

    for (i, g) in geom.iter().enumerate() {
        // La primera fila de listado lleva la primera entrada visible.
        let esperada = nombre_visible(&app, i, g.offset);
        assert!(
            fila_contiene(&buf, g.first_list_row, g.x, g.width, &esperada),
            "pane {i}: la fila {} debería llevar {esperada:?}",
            g.first_list_row
        );
        // La fila JUSTO ENCIMA es cromo (cabecera de columnas), nunca listado.
        assert!(
            !fila_contiene(&buf, g.first_list_row - 1, g.x, g.width, &esperada),
            "pane {i}: la cabecera no puede llevar contenido de listado"
        );
        // Y la fila justo DEBAJO de la última de listado es el borde.
        let bajo = g.first_list_row + g.list_rows;
        assert!(
            fila_es_borde(&buf, bajo, g.x, g.width),
            "pane {i}: la fila {bajo} debería ser el borde inferior"
        );
    }
}
```

Write `app_de_prueba_con`, `pintar`, `nombre_visible`, `fila_contiene` and
`fila_es_borde` as local helpers in this file, built on whatever
`crates/norte-tui/tests/render.rs` already does. Keep them in this file: it is
the only consumer, and a shared fixture touched by three crates is how a green
`just t norte-testkit` shipped with three red archive providers.

- [ ] **Step 3: Run it and watch it PASS**

```sh
just t norte-tui
```

Expected: PASS. This is the unusual case where a new test going green
immediately is the point — it is a characterisation test of code that already
works. If it fails, the helpers are wrong, not the production code.

- [ ] **Step 4: Take the `orthodox` snapshot baseline**

Add to the same file a snapshot of the whole painted screen at 100×30 with a
fixed listing, following the pattern in
`crates/norte-tui/tests/snapshots_ui.rs` (the repository already uses `insta`
and stores accepted snapshots under `crates/norte-tui/tests/snapshots/`).

```rust
/// El criterio de aceptación de L1a, escrito como test: esta pantalla es
/// idéntica antes y después del refactor. Si cambia una celda, o el refactor
/// movió algo o alguien cambió el render a propósito — y entonces se acepta el
/// snapshot nuevo A MANO, nunca con `--accept` a ciegas.
#[test]
fn la_pantalla_orthodox_no_se_mueve() {
    let (mut app, area) = app_de_prueba_con(60, 100, 30);
    let buf = pintar(&mut app, area);
    insta::assert_snapshot!("orthodox-100x30", buffer_a_texto(&buf));
}
```

- [ ] **Step 5: Accept the baseline and commit**

`cargo-insta` **no está instalado en esta máquina**, así que aceptar un
snapshot es mover el fichero a mano: borrar la línea `assertion_line:` de la
cabecera (cambia con cada edición del test y ensuciaría el diff) y renombrar
`.snap.new` a `.snap`. Míralo antes de aceptarlo.

```sh
just t norte-tui        # falla: snapshot nuevo sin aceptar
sed -i '/^assertion_line:/d' crates/norte-tui/tests/snapshots/layout_anchor__orthodox-100x30.snap.new
mv crates/norte-tui/tests/snapshots/layout_anchor__orthodox-100x30.snap{.new,}
just t norte-tui        # PASS
git add crates/norte-tui/tests/layout_anchor.rs crates/norte-tui/tests/snapshots/
git commit -m "test(tui): anchor the painted geometry and the orthodox screen"
```

---

### Task 2: The tree types

**Files:**
- Create: `crates/norte-frontend/src/layout/mod.rs`, `crates/norte-frontend/src/layout/tree.rs`
- Modify: `crates/norte-frontend/src/lib.rs` (add `pub mod layout;` in alphabetical order, between `keysheet` and `modal`)

- [ ] **Step 1: Write the failing tests**

In `tree.rs`, under `#[cfg(test)] mod tests`:

```rust
/// El árbol hace round-trip: es el MISMO formato que el fichero de config, el
/// blob de sesión de L2 y lo que escupirá el editor de layouts. Un formato que
/// no round-trippea obliga a migrar entre dos, que es justo lo que la ADR 0058
/// evita.
#[test]
fn el_arbol_hace_round_trip() {
    let arbol = Node::Split {
        dir: Dir::Horizontal,
        weights: vec![1, 1],
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
    assert!(vuelta.contains("\"shell\":\"fish\""), "los params se pierden: {vuelta}");
}

/// Los ids visibles de un árbol, en orden de lectura. Lo usan roles, store y
/// resolve, así que se prueba aquí una vez.
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
```

- [ ] **Step 2: Run and watch them fail**

```sh
just t norte-frontend
```

Expected: FAIL to compile — `layout` does not exist.

- [ ] **Step 3: Write the types**

Exact signatures, because everything downstream is written against them:

```rust
/// Un rectángulo en CELDAS. Propio y no el de ratatui: `norte-frontend` no
/// depende de ningún toolkit, y la GUI escala estas celdas por su métrica de
/// fuente. Los campos se llaman igual que los de `ratatui::layout::Rect` a
/// propósito, para que la conversión en el TUI sea campo a campo y sin
/// interpretación.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect { pub x: u16, pub y: u16, pub width: u16, pub height: u16 }

/// Identidad de un hueco. Se acuña por layout y NO se reutiliza dentro de una
/// sesión: cerrar un hueco deja su estado huérfano en el [`crate::layout::store::SlotStore`],
/// para que reabrir la misma disposición recupere el historial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SlotId(pub u32);

/// Qué hay dentro de un hueco. STRING y no enum: un enum cierra el registro, y
/// con él la puerta a que un plugin aporte un kind (ADR 0058 D2).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct KindId(String);

impl KindId {
    /// Un kind cualquiera, por nombre.
    pub fn new(s: impl Into<String>) -> Self;
    /// El kind del listado de ficheros: el único que este plan instancia.
    #[must_use] pub fn browser() -> Self;
    /// El nombre, para la tabla de renderers de cada frontend.
    #[must_use] pub fn as_str(&self) -> &str;
}

/// Dirección de un [`Node::Split`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dir { Horizontal, Vertical }

/// Parámetros de un hueco: bolsa OPACA que solo interpreta su kind. El motor
/// no la lee nunca — es la mitad cliente de la misma decisión que impide al
/// core leerla (ADR 0058 D4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Params(serde_json::Map<String, serde_json::Value>);

/// A quién sigue un hueco. Sin esto, un panel auxiliar es una caja sin nada
/// dentro.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Follow { Role(RoleId), Slot(SlotId) }

/// Los vínculos de un hueco.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bindings { pub follows: Option<Follow> }

/// Un puntero con nombre dentro del árbol, resuelto en cada frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleId { Active, Target }

/// Un nodo del árbol. Tres variantes y ni una más: las pestañas son un TIPO DE
/// NODO, no una feature, así que dónde caen decide si son espacios de trabajo,
/// pestañas de panel o media pantalla alternando vistas (ADR 0058 D1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Node {
    Split { dir: Dir, children: Vec<Node>, weights: Vec<u16> },
    Tabs { children: Vec<Node>, active: usize },
    Slot { id: SlotId, kind: KindId, #[serde(default)] params: Params, #[serde(default)] bindings: Bindings },
}

impl Node {
    /// Un hueco sin params ni vínculos. Atajo de construcción, muy usado por
    /// los tests y por los presets compilados.
    #[must_use] pub fn slot(id: SlotId, kind: KindId) -> Self;
    /// Todos los ids del árbol en orden de lectura, INCLUIDOS los de pestañas
    /// no activas: un hueco oculto sigue existiendo y sigue teniendo estado.
    #[must_use] pub fn slot_ids(&self) -> Vec<SlotId>;
}
```

`mod.rs` carries the module rustdoc (say what a slot is and point at ADR 0058),
`pub use` for the types above, and the two error kinds:

```rust
/// Lo que impide usar un layout. Se distingue del diagnóstico igual que el
/// keymap distingue [`crate::keymap::KeymapError`] de
/// [`crate::keymap::KeymapDiagnostic`].
#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    #[error("dos huecos con el mismo id: {0:?}")]
    DuplicateSlotId(SlotId),
    #[error("un `Split` sin hijos")]
    EmptySplit,
    #[error("{count} pesos para {children} hijos")]
    WeightsMismatch { count: usize, children: usize },
}

/// Lo que se arregla solo y hay que CONTAR. `norte doctor` los muestra.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutDiagnostic {
    ActiveClamped { was: usize, to: usize },
    ZeroWeightRaised { at: usize },
    FollowRetargeted { slot: SlotId },
}
```

`serde_json` must be added to `crates/norte-frontend/Cargo.toml` if it is not
already a dependency — check first; `Params` needs it. Justify it in the commit
message per hard rule 8 if it is new (it is almost certainly already in the
graph through `norte-proto`).

- [ ] **Step 4: Run the tests**

```sh
just t norte-frontend
cargo doc -p norte-frontend --no-deps
```

Expected: PASS, and no intra-doc link warnings.

- [ ] **Step 5: Commit**

```sh
git add crates/norte-frontend/src/layout crates/norte-frontend/src/lib.rs crates/norte-frontend/Cargo.toml
git commit -m "feat(frontend): the layout tree, and a kind that is a string"
```

---

### Task 3: The kind registry

**Files:**
- Create: `crates/norte-frontend/src/layout/kinds.rs`
- Modify: `crates/norte-frontend/src/layout/mod.rs` (declare and re-export)

- [ ] **Step 1: Write the failing tests**

```rust
/// Un kind que el registro no conoce no revienta: devuelve `None` y quien
/// pinta dibuja la caja con el nombre. Es la regla 3 del modelo.
#[test]
fn un_kind_fuera_del_registro_no_es_un_error() {
    let reg = KindRegistry::builtin();
    assert!(reg.get(&KindId::new("terminal")).is_none());
}

/// Los mínimos son lo único que el motor consulta para colapsar, así que
/// declararlos mal se nota en toda la pantalla.
#[test]
fn el_browser_declara_su_minimo_y_puede_tomar_los_dos_roles() {
    let reg = KindRegistry::builtin();
    let d = reg.get(&KindId::browser()).expect("browser está");
    assert_eq!(d.min, (20, 5));
    assert!(d.focusable && d.takes_keys && d.multi);
    assert_eq!(d.roles, &[RoleId::Active, RoleId::Target]);
}

/// `tasks` es la franja de abajo: no toma foco, no toma teclas, y hay UNA.
#[test]
fn tasks_es_unico_y_no_toma_foco() {
    let reg = KindRegistry::builtin();
    let d = reg.get(&KindId::new("tasks")).expect("tasks está");
    assert!(!d.focusable && !d.takes_keys && !d.multi);
    assert!(d.roles.is_empty());
}
```

- [ ] **Step 2: Run and watch them fail**

```sh
just t norte-frontend
```

Expected: FAIL — `KindRegistry` does not exist.

- [ ] **Step 3: Implement**

```rust
/// Lo que el motor necesita saber de un kind. NO incluye cómo se pinta: eso es
/// una tabla por frontend, porque el TUI pinta ratatui y la GUI pinta GPUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindDecl {
    pub id: KindId,
    /// Ancho y alto MÍNIMOS en celdas. Por debajo de esto, `resolve` colapsa.
    pub min: (u16, u16),
    pub focusable: bool,
    pub takes_keys: bool,
    /// ¿Pueden coexistir varias instancias?
    pub multi: bool,
    /// A qué roles puede optar este kind.
    pub roles: &'static [RoleId],
}

/// Los kinds que este binario sabe pintar. Abierto por construcción: `get`
/// devuelve `None` para lo que no conoce y eso NO es un error.
#[derive(Debug, Clone, Default)]
pub struct KindRegistry { /* Vec<KindDecl> */ }

impl KindRegistry {
    /// Los cinco kinds que existen hoy, re-encuadrados: `browser`, `tasks`,
    /// `viewer`, `compare`, `sync`.
    #[must_use] pub fn builtin() -> Self;
    #[must_use] pub fn get(&self, id: &KindId) -> Option<&KindDecl>;
    /// Añade uno (lo usará L1b para los kinds de L3 y, más tarde, un plugin).
    pub fn insert(&mut self, decl: KindDecl);
}
```

Minimums to declare, taken from what today's screen actually needs:
`browser (20, 5)`, `tasks (20, 3)`, `viewer (20, 5)`, `compare (40, 8)`,
`sync (40, 8)`. `viewer`, `compare` and `sync` are `focusable`, `takes_keys`,
not `multi`, and hold no roles. Only `browser` holds `[Active, Target]`.

- [ ] **Step 4: Run the tests**

```sh
just t norte-frontend
```

Expected: PASS.

- [ ] **Step 5: Commit**

```sh
git add crates/norte-frontend/src/layout
git commit -m "feat(frontend): what the engine needs to know about a kind"
```

---

### Task 4: `resolve` — the whole engine

The one function everything reads. Table tests plus properties.

**Files:**
- Create: `crates/norte-frontend/src/layout/resolve.rs`
- Modify: `crates/norte-frontend/src/layout/mod.rs`

- [ ] **Step 1: Write the failing tests**

```rust
fn reg() -> KindRegistry { KindRegistry::builtin() }
fn r(x: u16, y: u16, w: u16, h: u16) -> Rect { Rect { x, y, width: w, height: h } }

/// El caso `orthodox`: dos browsers al 50 % y la franja de tasks. Es la
/// pantalla de hoy expresada en el modelo nuevo, y que sea expresable es la
/// prueba de que el modelo está bien puesto.
#[test]
fn dos_browsers_al_cincuenta_por_ciento() {
    let arbol = Node::Split {
        dir: Dir::Horizontal, weights: vec![1, 1],
        children: vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(2), KindId::browser()),
        ],
    };
    let out = resolve(r(0, 0, 100, 30), &arbol, &reg());
    assert_eq!(out.placements, vec![(SlotId(1), r(0, 0, 50, 30)), (SlotId(2), r(50, 0, 50, 30))]);
    assert!(out.hidden.is_empty());
    assert_eq!(out.focus_order, vec![SlotId(1), SlotId(2)]);
}

/// Un ancho impar no puede perder una columna: el reparto reparte el resto.
#[test]
fn un_ancho_impar_no_pierde_una_columna() {
    let arbol = Node::Split {
        dir: Dir::Horizontal, weights: vec![1, 1],
        children: vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(2), KindId::browser()),
        ],
    };
    let out = resolve(r(0, 0, 101, 30), &arbol, &reg());
    let ancho: u16 = out.placements.iter().map(|(_, re)| re.width).sum();
    assert_eq!(ancho, 101, "se perdió una columna en el reparto");
}

/// Solo la pestaña ACTIVA se coloca; las otras van a `hidden`, que es la señal
/// de suspensión (fuera watches, sondas y columnas de plugin).
#[test]
fn una_pestana_inactiva_va_a_hidden_no_a_placements() {
    let arbol = Node::Tabs {
        active: 1,
        children: vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(2), KindId::browser()),
        ],
    };
    let out = resolve(r(0, 0, 100, 30), &arbol, &reg());
    assert_eq!(out.placements, vec![(SlotId(2), r(0, 0, 100, 30))]);
    assert_eq!(out.hidden, vec![SlotId(1)]);
    assert_eq!(out.focus_order, vec![SlotId(2)], "no se tabula a lo que no se ve");
}

/// El colapso: dos browsers de mínimo 20 no caben en 30 columnas, así que el
/// `Split` se degrada a `Tabs` PARA ESTE FRAME. El árbol de entrada no se toca.
#[test]
fn un_split_que_no_cabe_colapsa_a_pestanas_sin_tocar_el_arbol() {
    let arbol = Node::Split {
        dir: Dir::Horizontal, weights: vec![1, 1],
        children: vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(2), KindId::browser()),
        ],
    };
    let antes = arbol.clone();
    let out = resolve(r(0, 0, 30, 30), &arbol, &reg());
    assert_eq!(out.placements.len(), 1, "solo uno cabe");
    assert_eq!(out.hidden, vec![SlotId(2)]);
    assert_eq!(arbol, antes, "resolve NO puede mutar el árbol");
}

/// No cabe NADA: se pinta igual el primero, incumpliendo su mínimo. Nunca
/// pantalla en blanco.
#[test]
fn cuando_no_cabe_nada_se_pinta_uno_igualmente() {
    let arbol = Node::slot(SlotId(1), KindId::browser());
    let out = resolve(r(0, 0, 6, 2), &arbol, &reg());
    assert_eq!(out.placements, vec![(SlotId(1), r(0, 0, 6, 2))]);
    assert!(out.hidden.is_empty());
}

/// Un kind fuera del registro se coloca igual (caja con su nombre) pero no
/// entra en el orden de foco.
#[test]
fn un_kind_desconocido_se_coloca_pero_no_toma_foco() {
    let arbol = Node::slot(SlotId(9), KindId::new("terminal"));
    let out = resolve(r(0, 0, 100, 30), &arbol, &reg());
    assert_eq!(out.placements.len(), 1);
    assert!(out.focus_order.is_empty());
}

/// `active` fuera de rango se clampa y se CUENTA; no es un error duro.
#[test]
fn un_active_fuera_de_rango_se_clampa_con_diagnostico() {
    let arbol = Node::Tabs { active: 7, children: vec![Node::slot(SlotId(1), KindId::browser())] };
    let out = resolve(r(0, 0, 100, 30), &arbol, &reg());
    assert_eq!(out.placements.len(), 1);
    assert!(out.diagnostics.iter().any(|d| matches!(d, LayoutDiagnostic::ActiveClamped { .. })));
}
```

Then the four properties, in the same file, with `proptest` (already a workspace
dev-dependency — check `crates/norte-frontend/Cargo.toml` and add under
`[dev-dependencies]` if missing):

```rust
proptest! {
    /// Las colocaciones NUNCA se solapan y nunca se salen del área. Es la
    /// propiedad que impide que un panel pinte encima de otro, que en un TUI
    /// no se ve como un bug de layout sino como texto corrupto.
    #[test]
    fn las_colocaciones_ni_se_solapan_ni_se_salen(arbol in arbol_arbitrario(), w in 1u16..200, h in 1u16..80) {
        let out = resolve(Rect { x: 0, y: 0, width: w, height: h }, &arbol, &reg());
        for (i, (_, a)) in out.placements.iter().enumerate() {
            prop_assert!(a.x + a.width <= w && a.y + a.height <= h);
            for (_, b) in out.placements.iter().skip(i + 1) {
                prop_assert!(!se_solapan(*a, *b));
            }
        }
    }

    /// Todo hueco colocado cumple su mínimo, salvo en el caso «no cabe nada»
    /// (que se reconoce porque solo hay UNA colocación).
    #[test]
    fn todo_lo_colocado_cumple_su_minimo(arbol in arbol_arbitrario(), w in 1u16..200, h in 1u16..80) {
        let out = resolve(Rect { x: 0, y: 0, width: w, height: h }, &arbol, &reg());
        if out.placements.len() > 1 {
            for (id, a) in &out.placements {
                if let Some(min) = minimo_de(&arbol, *id, &reg()) {
                    prop_assert!(a.width >= min.0 && a.height >= min.1);
                }
            }
        }
    }

    /// `placements` y `hidden` PARTICIONAN los huecos del árbol: ni uno se
    /// queda sin clasificar, ni uno aparece en los dos. Si esto falla, un
    /// hueco vivo queda sin suspender y sin pintar — con su watch abierto.
    #[test]
    fn placements_y_hidden_particionan_el_arbol(arbol in arbol_arbitrario(), w in 1u16..200, h in 1u16..80) {
        let out = resolve(Rect { x: 0, y: 0, width: w, height: h }, &arbol, &reg());
        let mut vistos: Vec<SlotId> = out.placements.iter().map(|(id, _)| *id).collect();
        vistos.extend(out.hidden.iter().copied());
        vistos.sort_unstable();
        let mut todos = arbol.slot_ids();
        todos.sort_unstable();
        prop_assert_eq!(vistos, todos);
    }

    /// `focus_order` es un subconjunto EXACTO de lo colocado y enfocable.
    #[test]
    fn el_orden_de_foco_solo_lleva_visibles_enfocables(arbol in arbol_arbitrario(), w in 1u16..200, h in 1u16..80) {
        let out = resolve(Rect { x: 0, y: 0, width: w, height: h }, &arbol, &reg());
        let colocados: Vec<SlotId> = out.placements.iter().map(|(id, _)| *id).collect();
        for id in &out.focus_order {
            prop_assert!(colocados.contains(id));
        }
    }
}
```

`arbol_arbitrario()` is a `proptest` strategy that builds trees up to depth 3
from the built-in kinds plus one unknown kind, with 1–4 children per node and
weights in `1..=4`. Write it in this file; do not put it in `norte-testkit` —
it has exactly one consumer, and a shared fixture is not scoped by its crate.

- [ ] **Step 2: Run and watch them fail**

```sh
just t norte-frontend
```

Expected: FAIL — `resolve` does not exist.

- [ ] **Step 3: Implement**

```rust
/// Lo que un frame necesita saber, en un solo valor: quién se pinta y dónde,
/// quién NO se pinta (y por tanto se suspende), y en qué orden se tabula.
///
/// Que sea uno y no tres es la razón de ser del módulo: hoy `ui.rs` calcula el
/// reparto al pintar y `pane_geometry`/`pane_list_rows` lo REPLICAN para el
/// ratón y la paginación. Tres copias de una regla es una regla que se arregla
/// una vez y sigue mal en las otras dos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub placements: Vec<(SlotId, Rect)>,
    pub hidden: Vec<SlotId>,
    pub focus_order: Vec<SlotId>,
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
pub fn resolve(area: Rect, tree: &Node, decls: &KindRegistry) -> Resolved
```

Implementation notes, in the order the recursion needs them:

1. **`Split`** — sum the weights; if any is `0`, raise it to `1` and record
   `ZeroWeightRaised`. Distribute the main axis proportionally and hand the
   **remainder to the last child** so nothing is lost (the odd-width test).
2. **Minimum check** — before recursing, compute each child's minimum as the
   maximum over the minimums of the slots beneath it (a `Split` sums along its
   own axis, a `Tabs` takes the max). If the assigned extent is below it, the
   `Split` collapses: treat it as `Tabs { active: 0 }` for this frame.
3. **`Tabs`** — clamp `active` into range, recording `ActiveClamped`. The active
   child recurses into the whole area; **every slot under the other children
   goes to `hidden`** (use `Node::slot_ids`).
4. **`Slot`** — one placement. Push to `focus_order` only when the registry
   knows the kind and it is `focusable`.
5. **Nothing fits** — if the recursion would produce zero placements, place the
   first slot in reading order into the whole `area` regardless of its minimum.

- [ ] **Step 4: Run the tests**

```sh
just t norte-frontend
cargo doc -p norte-frontend --no-deps
```

Expected: PASS.

- [ ] **Step 5: Commit**

```sh
git add crates/norte-frontend/src/layout crates/norte-frontend/Cargo.toml
git commit -m "feat(frontend): one resolve, read by the painter, the mouse and the window"
```

---

### Task 5: Focus traversal

**Files:**
- Create: `crates/norte-frontend/src/layout/focus.rs`
- Modify: `crates/norte-frontend/src/layout/mod.rs`

- [ ] **Step 1: Write the failing tests**

```rust
fn resuelto(orden: &[u32]) -> Resolved {
    Resolved {
        placements: orden.iter().map(|i| (SlotId(*i), Rect { x: 0, y: 0, width: 10, height: 10 })).collect(),
        hidden: vec![],
        focus_order: orden.iter().map(|i| SlotId(*i)).collect(),
        diagnostics: vec![],
    }
}

#[test]
fn el_foco_cicla_en_los_dos_sentidos() {
    let r = resuelto(&[1, 2, 3]);
    assert_eq!(focus_next(&r, SlotId(3)), Some(SlotId(1)));
    assert_eq!(focus_prev(&r, SlotId(1)), Some(SlotId(3)));
    assert_eq!(focus_next(&r, SlotId(1)), Some(SlotId(2)));
}

/// El foco estaba en un hueco que acaba de ocultarse (cambio de pestaña, o la
/// ventana encogió y colapsó): NO se pierde, cae al primero visible. Un foco
/// apuntando a algo que no se ve es un teclado que no hace nada.
#[test]
fn un_foco_que_ya_no_se_ve_cae_al_primer_visible() {
    let r = resuelto(&[2, 3]);
    assert_eq!(focus_next(&r, SlotId(1)), Some(SlotId(2)));
    assert_eq!(focus_prev(&r, SlotId(1)), Some(SlotId(2)));
}

#[test]
fn sin_nada_enfocable_no_hay_foco() {
    let r = resuelto(&[]);
    assert_eq!(focus_next(&r, SlotId(1)), None);
}
```

- [ ] **Step 2: Run and watch them fail**

```sh
just t norte-frontend
```

Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
/// El siguiente hueco enfocable, ciclando. Si `actual` ya no está en
/// `focus_order` (se ocultó su pestaña, o colapsó), devuelve el PRIMERO: el
/// foco no se pierde nunca mientras haya algo que enfocar.
#[must_use] pub fn focus_next(resolved: &Resolved, actual: SlotId) -> Option<SlotId>;
/// Como [`focus_next`], hacia atrás.
#[must_use] pub fn focus_prev(resolved: &Resolved, actual: SlotId) -> Option<SlotId>;
```

- [ ] **Step 4: Run the tests**

```sh
just t norte-frontend
```

Expected: PASS.

- [ ] **Step 5: Commit**

```sh
git add crates/norte-frontend/src/layout
git commit -m "feat(frontend): focus that never lands on what is not on screen"
```

---

### Task 6: Roles and bindings

**Files:**
- Create: `crates/norte-frontend/src/layout/roles.rs`
- Modify: `crates/norte-frontend/src/layout/mod.rs`

- [ ] **Step 1: Write the failing tests**

```rust
/// Con dos browsers, `target` es el otro. Eso es lo que hace que el layout
/// ortodoxo se comporte EXACTAMENTE como hoy con el concepto ya presente pero
/// invisible: `F5` copia al otro pane y nadie se entera de que hay roles.
#[test]
fn con_dos_browsers_el_destino_es_el_otro() {
    let (arbol, res) = dos_browsers();
    let mut roles = Roles::default();
    roles.reconcile(&arbol, &res, &reg(), SlotId(1));
    assert_eq!(roles.get(RoleId::Active), Some(SlotId(1)));
    assert_eq!(roles.get(RoleId::Target), Some(SlotId(2)));
}

/// Con UN solo browser no hay destino, y eso NO es un estado roto: la
/// operación que lo necesite pedirá una ruta. Adivinar aquí es cómo una copia
/// sale hacia un sitio que el usuario no tenía en la cabeza.
#[test]
fn con_un_solo_browser_no_hay_destino() {
    let (arbol, res) = un_browser();
    let mut roles = Roles::default();
    roles.reconcile(&arbol, &res, &reg(), SlotId(1));
    assert_eq!(roles.get(RoleId::Target), None);
}

/// El destino se OCULTA (cambio de pestaña): el rol se reubica al candidato
/// visible. Un destino detrás de una pestaña es pérdida de datos silenciosa.
#[test]
fn un_destino_que_se_oculta_se_reubica() {
    let (arbol, res) = tres_browsers_con_el_dos_oculto();
    let mut roles = Roles::default();
    roles.set(RoleId::Target, SlotId(2));
    roles.reconcile(&arbol, &res, &reg(), SlotId(1));
    assert_eq!(roles.get(RoleId::Target), Some(SlotId(3)), "se reubica al visible");
}

/// Con TRES browsers visibles y ninguno designado, no hay default: dos
/// candidatos no se desempatan solos.
#[test]
fn con_varios_candidatos_no_hay_destino_por_defecto() {
    let (arbol, res) = tres_browsers_visibles();
    let mut roles = Roles::default();
    roles.reconcile(&arbol, &res, &reg(), SlotId(1));
    assert_eq!(roles.get(RoleId::Target), None);
}

/// Un `follows` roto degrada a seguir al rol `active` — lo que se quería el
/// 95 % de las veces — y lo CUENTA.
#[test]
fn un_follow_a_un_hueco_que_no_existe_degrada_a_active() {
    let arbol = Node::Slot {
        id: SlotId(1), kind: KindId::new("metadata"), params: Params::default(),
        bindings: Bindings { follows: Some(Follow::Slot(SlotId(99))) },
    };
    let mut diags = vec![];
    let objetivo = resolve_follow(&arbol, SlotId(1), &Roles::con_active(SlotId(1)), &mut diags);
    assert_eq!(objetivo, Some(SlotId(1)));
    assert!(diags.iter().any(|d| matches!(d, LayoutDiagnostic::FollowRetargeted { .. })));
}
```

Write `dos_browsers`, `un_browser`, `tres_browsers_con_el_dos_oculto`,
`tres_browsers_visibles` and `reg` as local helpers returning
`(Node, Resolved)`, built by calling `resolve` on a hand-written tree.

- [ ] **Step 2: Run and watch them fail**

```sh
just t norte-frontend
```

Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
/// Los punteros con nombre dentro del árbol. `active` es el foco; `target` es
/// a dónde va una operación que necesita un segundo sitio.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roles(BTreeMap<RoleId, SlotId>);

impl Roles {
    #[must_use] pub fn get(&self, role: RoleId) -> Option<SlotId>;
    pub fn set(&mut self, role: RoleId, slot: SlotId);
    /// Deja los roles coherentes con lo que hay en pantalla. Se llama tras
    /// CADA `resolve`, porque un rol es una afirmación sobre el frame actual.
    ///
    /// - `active` pasa a ser `foco`, siempre.
    /// - `target` se conserva si sigue visible y elegible; si no, se reubica al
    ///   ÚNICO otro candidato visible, y si hay cero o varios, se queda sin
    ///   fijar. No se desempata solo: una copia hacia un destino que el usuario
    ///   no tenía en la cabeza es pérdida de datos silenciosa (ADR 0058 D7).
    pub fn reconcile(&mut self, tree: &Node, resolved: &Resolved, decls: &KindRegistry, foco: SlotId);
    /// Atajo para tests y para el arranque.
    #[must_use] pub fn con_active(slot: SlotId) -> Self;
}

/// A qué hueco mira el hueco `de`. `None` = no sigue a nadie.
///
/// Un `follows` a un hueco que ya no existe degrada a `Follow::Role(Active)` y
/// deja un [`LayoutDiagnostic::FollowRetargeted`]: es lo que se quería casi
/// siempre, y borrarlo en silencio deja un panel auxiliar mirando al vacío.
#[must_use]
pub fn resolve_follow(tree: &Node, de: SlotId, roles: &Roles, diags: &mut Vec<LayoutDiagnostic>) -> Option<SlotId>;
```

- [ ] **Step 4: Run the tests and the gate**

```sh
just t norte-frontend
cargo doc -p norte-frontend --no-deps
just ci-fast          # UNA vez — primera de las dos de este plan
```

Expected: PASS. The engine is complete and nobody uses it yet.

- [ ] **Step 5: Commit**

```sh
git add crates/norte-frontend/src/layout
git commit -m "feat(frontend): roles that only ever point at what is on screen"
```

---

### Task 7: `SlotStore`

**Files:**
- Create: `crates/norte-frontend/src/layout/store.rs`
- Modify: `crates/norte-frontend/src/layout/mod.rs`

- [ ] **Step 1: Write the failing tests**

```rust
/// Cerrar un hueco NO borra su estado: queda huérfano, para que reabrir la
/// misma disposición recupere el historial en vez de arrancar en blanco.
#[test]
fn cerrar_un_hueco_deja_su_estado_huerfano() {
    let mut s: SlotStore<u32> = SlotStore::default();
    s.insert(SlotId(1), 10);
    s.insert(SlotId(2), 20);
    let arbol = Node::slot(SlotId(1), KindId::browser());
    s.sync_with(&arbol);
    assert_eq!(s.get(SlotId(2)), Some(&20), "el estado del hueco cerrado sigue");
    assert_eq!(s.orphans(), vec![SlotId(2)]);
}

/// Los huérfanos tienen tope: sin él, abrir y cerrar paneles toda una sesión
/// crece sin fin. Se purga el MÁS ANTIGUO.
#[test]
fn los_huerfanos_tienen_tope_y_se_purga_el_mas_antiguo() {
    let mut s: SlotStore<u32> = SlotStore::with_orphan_cap(2);
    for i in 1..=4 { s.insert(SlotId(i), i); }
    s.sync_with(&Node::slot(SlotId(4), KindId::browser()));
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
    s.sync_with(&Node::slot(SlotId(1), KindId::browser()));
    let arbol = Node::Split {
        dir: Dir::Horizontal, weights: vec![1, 1],
        children: vec![Node::slot(SlotId(1), KindId::browser()), Node::slot(SlotId(2), KindId::browser())],
    };
    s.sync_with(&arbol);
    assert_eq!(s.get(SlotId(2)), Some(&20));
    assert!(s.orphans().is_empty());
}
```

- [ ] **Step 2: Run and watch them fail**

```sh
just t norte-frontend
```

Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
/// El estado de los paneles, indexado por hueco.
///
/// GENÉRICO sobre el estado, y cada frontend pone el suyo: `norte-frontend`
/// tiene las mitades puras (`pane`, `viewer`, `compare`, `sync`), pero el TUI
/// las envuelve en vistas propias y la GUI en otras distintas. Un enum concreto
/// aquí arrastraría los tipos de vista de un frontend al grafo del otro.
///
/// La elegibilidad para un rol NO necesita un trait sobre `P`: la decide el
/// `kind` del hueco en el árbol más el registro.
#[derive(Debug, Clone)]
pub struct SlotStore<P> { /* … */ }

impl<P> SlotStore<P> {
    /// Tope de huérfanos por defecto: 32. Generoso para una sesión de trabajo
    /// y acotado para que no crezca sin fin.
    #[must_use] pub fn with_orphan_cap(cap: usize) -> Self;
    pub fn insert(&mut self, id: SlotId, state: P);
    #[must_use] pub fn get(&self, id: SlotId) -> Option<&P>;
    pub fn get_mut(&mut self, id: SlotId) -> Option<&mut P>;
    /// Reclasifica: lo que el árbol menciona está VIVO, lo demás queda
    /// huérfano; si se pasa del tope, se purga el más antiguo. Se llama tras
    /// cada cambio de layout.
    pub fn sync_with(&mut self, tree: &Node);
    /// Los ids huérfanos, del más antiguo al más reciente.
    #[must_use] pub fn orphans(&self) -> Vec<SlotId>;
}
```

`Default` gives the 32 cap. Orphan age is insertion order into the orphan list,
not wall-clock time — there is no clock in this crate and none is wanted.

- [ ] **Step 4: Run the tests**

```sh
just t norte-frontend
```

Expected: PASS.

- [ ] **Step 5: Commit**

```sh
git add crates/norte-frontend/src/layout
git commit -m "feat(frontend): panel state keyed by slot, and closing one does not forget it"
```

---

### Task 8: The TUI stores its panels in the store

The wide, mechanical one. ~187 call sites. **The screen must not move.**

**Files:**
- Create: `crates/norte-tui/src/panel.rs`
- Modify: `crates/norte-tui/src/app.rs` (the `App` struct and its accessors), plus every site that reaches `app.panes[..]`
- Modify: `crates/norte-tui/src/lib.rs` (declare `mod panel;`)

- [ ] **Step 1: Define the TUI's panel state and the fixed ids**

```rust
//! El estado de panel del TUI, y los dos huecos que sustituyen a `panes[2]`.

/// Los ids bien conocidos del preset `orthodox`. Fijos y no acuñados porque
/// son los mismos huecos que el TUI ha tenido siempre: así el estado de hoy y
/// el preset son la misma cosa, sin migración.
pub const SLOT_LEFT: SlotId = SlotId(1);
pub const SLOT_RIGHT: SlotId = SlotId(2);
pub const SLOT_TASKS: SlotId = SlotId(3);

/// Lo que puede haber dentro de un hueco EN EL TUI.
pub enum TuiPanel {
    Browser(Box<crate::app::Pane>),
    Viewer(Box<crate::viewer::Viewer>),
    Tasks(Box<crate::tasks::TaskBoard>),
    Compare(Box<crate::app::CompareView>),
    Sync(Box<crate::app::SyncView>),
    Unknown { kind: KindId, raw: Params },
}
```

Box the variants: `Pane` and `SyncView` are large, and an unboxed enum is as big
as its largest variant everywhere it is stored. `clippy::large_enum_variant` is
denied in this workspace and will tell you so.

- [ ] **Step 2: Add the store to `App` behind accessors that return today's types**

```rust
// en App
/// El estado de los paneles. `panes` desaparece; los dos de siempre son
/// `SLOT_LEFT` y `SLOT_RIGHT`.
pub slots: norte_frontend::layout::SlotStore<crate::panel::TuiPanel>,
/// El layout vigente. En L1a siempre es `orthodox`.
pub layout: norte_frontend::layout::Node,
/// Los roles, reconciliados tras cada `resolve`.
pub roles: norte_frontend::layout::Roles,
/// Los kinds que este binario sabe pintar. `KindRegistry::builtin()` en L1a;
/// L1b le añadirá los de L3 y, más tarde, los de un plugin.
pub kinds: norte_frontend::layout::KindRegistry,

impl App {
    /// El pane izquierdo. Existe para que los ~187 sitios que decían
    /// `app.panes[0]` sean un cambio de acceso y no un rediseño.
    pub fn left(&self) -> &Pane;
    pub fn left_mut(&mut self) -> &mut Pane;
    pub fn right(&self) -> &Pane;
    pub fn right_mut(&mut self) -> &mut Pane;
    /// El pane con el foco, que es lo que antes se llamaba «el activo».
    pub fn focused_pane(&self) -> &Pane;
    pub fn focused_pane_mut(&mut self) -> &mut Pane;
    /// El pane de DESTINO, si lo hay. `None` con un solo browser — y eso no es
    /// un error: quien lo necesite pide una ruta.
    pub fn target_pane(&self) -> Option<&Pane>;
}
```

The accessors `expect()` on the two well-known ids. That is one of the rare
places hard rule 6 allows it, and the comment must state the invariant: *the
`orthodox` layout always contains `SLOT_LEFT` and `SLOT_RIGHT`, and L1a builds
no other layout.*

- [ ] **Step 3: Rewrite the call sites, mechanically**

```sh
grep -rn "panes\[0\]\|panes\[1\]\|\.panes\b" crates/norte-tui/src | wc -l
```

Translate: `app.panes[0]` → `app.left()`/`app.left_mut()`, `app.panes[1]` →
`app.right()`/`app.right_mut()`, and iterations over `app.panes` → an explicit
pair. Where a site indexes by a computed side (`app.panes[i]`), add
`fn pane(&self, side: usize) -> &Pane` rather than leaking `SlotId` arithmetic
into call sites — that generality is L1b's job.

**Do not change behaviour anywhere in this step.** If a site looks wrong, note
it and leave it; a bug fix hidden inside a 187-site refactor is a bug fix nobody
reviewed.

- [ ] **Step 4: Run the anchor and the snapshot**

```sh
just t norte-tui
```

Expected: PASS, **including the `orthodox` snapshot unchanged**. If the snapshot
moved, the refactor changed the screen — find out why. Do not accept the new
snapshot.

- [ ] **Step 5: Clippy and commit**

```sh
just c
git add crates/norte-tui/src
git commit -m "refactor(tui): panes live in the slot store, and the screen does not move"
```

---

### Task 9: The TUI paints from `resolve`

**Files:**
- Modify: `crates/norte-tui/src/ui.rs` (`draw`, `before_frame`, `pane_geometry`, `pane_list_rows`, `overlay_body`)
- Modify: `crates/norte-tui/src/mouse.rs` (`after_frame` takes `Resolved`)
- Modify: `crates/norte-tui/tests/layout_anchor.rs` (re-aim at `Resolved`)

- [ ] **Step 1: Add the `orthodox` preset and the `Rect` conversion**

In `crates/norte-tui/src/panel.rs`:

```rust
/// El preset por defecto, que es EXACTAMENTE la pantalla de hoy: dos browsers
/// al 50 % con la franja de tasks debajo. Que la pantalla de siempre sea
/// expresable en el modelo nuevo es la prueba de que el modelo está bien
/// puesto.
#[must_use]
pub fn orthodox() -> norte_frontend::layout::Node;

/// Celdas a `ratatui::layout::Rect`, campo a campo, y de vuelta. Los nombres
/// coinciden a propósito: aquí no hay interpretación que hacer.
#[must_use]
pub fn to_ratatui(r: norte_frontend::layout::Rect) -> ratatui::layout::Rect;
#[must_use]
pub fn from_ratatui(r: ratatui::layout::Rect) -> norte_frontend::layout::Rect;
```

- [ ] **Step 2: Make `draw` resolve once and paint from the result**

`draw` computes `let resolved = resolve(from_ratatui(area), &app.layout, &app.kinds);`
**once**, then paints each placement through a `match` on the slot's kind. The
task strip and the status bar stay where they are for now: in L1a `orthodox`
places them, and the arithmetic that used to live in `overlay_body` becomes the
weights of the `orthodox` tree.

`before_frame` reads `resolved.placements` for the browser slots instead of
calling `pane_list_rows`, and suspends everything in `resolved.hidden` — in L1a
that list is always empty, so add the loop with a comment saying it is the seam
L1b fills, and a test that it is a no-op today.

- [ ] **Step 3: Retire the duplicated geometry**

`pane_geometry` and `pane_list_rows` are deleted. `mouse::after_frame` takes
`&Resolved` and builds its `PaneGeometry` from the placement plus the chrome
arithmetic that already lives in it (border, column header). Keep
`PaneGeometry` — the mouse hit test is unchanged; only where it gets its
rectangle from moves.

- [ ] **Step 4: Re-aim the anchor test and run everything**

The anchor test's assertion does not change; it now reads its rectangles from
`Resolved` instead of `pane_geometry`. This is the moment the test earns its
existence.

```sh
just t norte-tui
```

Expected: PASS, **and the `orthodox` snapshot still unchanged**.

- [ ] **Step 5: The gate, then commit**

```sh
just c
just ci-fast          # la SEGUNDA y última de este plan
git add crates/norte-tui/src crates/norte-tui/tests
git commit -m "refactor(tui): one resolve feeds the painter, the mouse and the window"
```

---

### Task 10: Close the branch

- [ ] **Step 1: Review before committing anything else**

Dispatch `rust-reviewer` over the whole branch range with the commit range, what
the change is for, and the two questions worth asking: *does any call site
change behaviour rather than only its access path*, and *is the `expect()` in
the well-known-slot accessors justified by an invariant L1b will not break*.
Apply BLOCKER and MAJOR findings in ONE pass. Say which MINORs were skipped and
why.

`protocol-guardian` is **not** needed: this plan touches no wire type.
`security-reviewer` is not needed: no journal, policy, auth or plugin surface.

- [ ] **Step 2: The full gate, once**

```sh
just ci
```

Run the recipes one at a time in the foreground if it is killed at five minutes,
and never through `| tail`.

- [ ] **Step 3: Changelog**

Add an `### Changed` entry under `## [Unreleased]` in `CHANGELOG.md`. It is a
refactor with no user-visible effect, and the entry should say exactly that —
that the screen is now built from a layout the user will be able to change, and
that nothing about it has moved yet.

- [ ] **Step 4: Update the memory and finish the branch**

Update `layout-huecos-pestanas.md` in the memory directory with what L1a
actually cost and anything it discovered, then use
`superpowers:finishing-a-development-branch`.

---

## What actually happened

Recorded here because the plan was wrong about the two expensive parts, and the
next plan should not repeat it.

- **Task 8 was not 213 call sites; it was one.** Rewriting them all would have
  moved the storage and, in the commit that promises not to change behaviour,
  offered 213 chances to change it. `PaneSlots` is a `SlotStore` behind an
  adapter that still indexes by side, iterates and swaps like the array did, so
  everything compiled untouched except `pane_read_only` — whose index comes
  from outside and wants a `None`, not a panic. **When a plan says "N mechanical
  sites", ask first whether an adapter makes N zero.**
- **The orthodox tree covers the body, not the frame.** A `Split` only divides
  proportionally; the status bar is a fixed height and the task strip is sized
  by its contents. The plan's line about `overlay_body`'s arithmetic "becoming
  the weights of the orthodox tree" was not achievable. See the spec's *Sizing*
  section — it is L1b's first item, before tabs, because it changes the stored
  format.
- **`TuiPanel` has two live variants, not five.** The viewer, compare and sync
  views stay `App` fields until L1b: moving them is not needed for the engine to
  land.
- **`pane_list_rows` did not retire.** It computes vertical chrome, which is not
  in the tree for the reason above. `pane_geometry` did stop computing its own
  split, which was the duplication that mattered.
- **`cargo-insta` is not installed here**; accepting a snapshot is a manual move.
- **The doc-link trap fired three times** — `SlotStore`, `resolve` and
  `pane_rects` — all of them green under `just t` and `just c`. `cargo doc -p
  <crate> --no-deps` after every task, not at the end.

## L1b, so nobody builds it here

Left for the next plan, in this order:

1. `Tabs` construction and the `tab.*` commands; hidden-slot suspension made
   real (drop watches and probes) with its `MemProvider` test, and the
   cancellation test for closing a slot with a listing in flight.
2. `layout.*` commands, `layout.set-target`, and the target drawn in the chrome.
3. The seven keymap presets, the reference sheet, which-key and the help — the
   core bound, the rest shipped unbound and greyed.
4. `[ui.layout]` and `~/.config/norte/layouts/<name>.toml`.
5. The GUI: its own renderer table over the same `resolve`, behind its own gate
   (`just gui-ci`).
