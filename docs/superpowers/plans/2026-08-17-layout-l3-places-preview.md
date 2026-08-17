# L3 — a places sidebar and a viewer that follows the cursor

> **For agentic workers:** REQUIRED SUB-SKILL: `superpowers:executing-plans`.
> Steps are checkboxes. Nothing here notifies you: no `sleep`, no
> `tail -f /dev/null`, no waiting on a monitor that does not exist.

**Goal:** put the first non-`browser` panels in slots — a places sidebar and a
docked viewer bound to the active listing — and in doing so give
`Bindings::follows` and `Size::Fixed` their first real consumers.

**Design:** `docs/superpowers/specs/2026-08-17-layout-l3-places-preview-design.md`
**Decision:** ADR 0058. **Predecessors:** the L1a, L1b and P6 plans — read their
*What actually happened* sections before touching anything.

**Wire:** untouched. No `norte-proto` change, no protocol bump, no
`protocol-guardian`.

## What is actually in the way

Three things, none of them the rendering.

1. **There is no tree operation that docks a panel at the edge of a split.**
   `split_slot` cuts one slot in two; `add_tab` wraps. Neither puts a
   `Fixed(16)` sidebar to the left of *both* browsers. Stage A adds one.
2. **`App.focus` is an index over visible browsers.** A sidebar that takes keys
   cannot hold that focus without the `focus: SlotId` refactor P6 deferred. It
   does not need to: the sidebar is an overlay-shaped keyboard owner, exactly
   like the help panel, and `App.focused()` keeps meaning *the listing you were
   in*. Stage B adds `KeyOwner`.
3. **`open_viewer` is a modal fetch.** It `select!`s over the event stream so
   Esc can abandon it. A docked preview reads while you keep pressing arrows,
   so the read must become a per-slot background fetch like `fill` and
   `decorate_fetch` — the `BySlot` machinery P6 stage C built. Stage D.

## The rule that shapes the testing

`main.rs` is a binary: integration tests cannot reach the run loop. So **the
decisions live in the lib and only the I/O lives in `main.rs`.** In particular
"what should the preview be showing right now" is a pure function of the tree,
the roles and the cursor, and that is where every rule in the spec gets pinned:
a hidden slot produces no target, so no request exists to count.

## Stages

| stage | what | tasks |
| --- | --- | --- |
| **A** | `Edge` + `Node::dock` in the engine | 1 |
| **B** | `places` state, kind, `TuiPanel::Places`, `KeyOwner`, toggle | 2–4 |
| **C** | sidebar keys, activation, drive refresh, surface (catalogue/i18n/menu/presets) | 5–6 |
| **D** | the read extracted, `TuiPanel::Viewer`, per-slot fetch | 7–8 |
| **E** | preview rendering, rules, surface | 9–10 |
| **F** | changelog, memory, gate | 11 |

**Gate budget (CLAUDE.md):** `just t <crate>` freely during RED→GREEN.
`just ci-fast` **once** after task 4 and **once** after task 8. `just ci`
**once** at task 11. Never re-run the gate to see whether a fix worked.
After every task that touches a documented item: `cargo doc -p <crate>
--no-deps` — the intra-doc link lint fired three times in L1a with `just t` and
`just c` both green.

---

## Task 1: `Node::dock` — a panel at the edge of a split

**Files:**
- Modify: `crates/norte-frontend/src/layout/tree.rs`
- Modify: `crates/norte-frontend/src/layout/mod.rs` (re-export `Edge`)

The operation: dock `nuevo` at edge `edge` of the **nearest ancestor `Split` of
`anchor` whose direction matches the edge's axis**. `Left`/`Top` insert at the
front, `Right`/`Bottom` at the back, with `size`. If no ancestor runs that
axis, wrap the anchor's whole subtree in a new `Split` of that axis with the
anchor weighted `Weight(1)`.

For `orthodox` that means: anchor = a browser, `Edge::Left` → the body's
`Split(Horizontal)` gets a new first child, so the sidebar sits left of both
listings and *above* the tasks strip and the status bar. That is the whole
reason the operation targets an ancestor split instead of the root.

Undocking needs nothing new: `close_slot` already removes a slot and collapses
the one-child split it leaves behind.

- [ ] **Step 1: write the failing tests**

```rust
// en el mod tests de tree.rs
/// Un dock a la izquierda entra en el Split HORIZONTAL que ya existe, no
/// alrededor del árbol entero: si envolviera la raíz, la barra de estado y la
/// franja de tareas se quedarían a la derecha del sidebar.
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
    let Node::Split { children: cuerpo, sizes, dir } = &children[0] else {
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
    let Node::Split { children, .. } = &con else { panic!("split") };
    assert_eq!(children.len(), 3);
    assert_eq!(children[2].first_slot_id(), Some(SlotId(9)));
}

/// Sin ancestro en el eje pedido, se ENVUELVE. Un solo pane es el caso real:
/// tras cerrar uno, el cuerpo puede ser una hoja suelta.
#[test]
fn sin_ancestro_en_el_eje_se_envuelve() {
    let arbol = Node::slot(SlotId(1), KindId::browser());
    let con = arbol.dock(
        SlotId(1),
        Edge::Left,
        Size::Fixed(16),
        &Node::slot(SlotId(9), KindId::new("places")),
    );
    let Node::Split { dir, children, sizes } = &con else { panic!("envuelto en Split") };
    assert_eq!(*dir, Dir::Horizontal);
    assert_eq!(children[0].first_slot_id(), Some(SlotId(9)));
    assert_eq!(children[1].first_slot_id(), Some(SlotId(1)));
    assert_eq!(sizes, &vec![Size::Fixed(16), Size::Weight(1)]);
}

/// Un ancla que no está en el árbol no inventa nada: el árbol vuelve igual.
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

/// Y deshacerlo es `close_slot`, que ya existe: el Split de un solo hijo
/// colapsa y el árbol vuelve a ser el de antes. Esto es lo que hace que el
/// toggle sea reversible de verdad y no deje un Split degenerado por sesión.
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
```

- [ ] **Step 2: run them and watch them fail**

`just t norte-frontend` — expected: does not compile, `Edge` and `dock` do not
exist.

- [ ] **Step 3: implement**

```rust
/// Un borde de la pantalla, para acoplar un panel contra él.
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
    pub const fn axis(self) -> Dir { /* Left|Right => Horizontal, ... */ }
    /// ¿Va delante de los que ya están?
    #[must_use]
    pub const fn is_front(self) -> bool { /* Left|Top */ }
}
```

`Node::dock(&self, anchor: SlotId, edge: Edge, size: Size, nuevo: &Self) -> Self`
recurses like `split_slot` does: rebuild the tree, and at the deepest `Split`
that contains `anchor` **and** runs `edge.axis()`, insert. If the recursion
reaches the anchor without having found such a split, the caller wraps. Follow
`split_slot`'s existing shape rather than inventing a second traversal style.

- [ ] **Step 4: green, and the doc link check**

`just t norte-frontend`, then `cargo doc -p norte-frontend --no-deps`.

- [ ] **Step 5: commit**

```bash
git add crates/norte-frontend/src/layout/
git commit -m "feat(layout): a panel can be docked against the edge of a split"
```

---

## Task 2: `places.rs` — the sidebar's state, with no I/O in it

**Files:**
- Create: `crates/norte-frontend/src/places.rs`
- Modify: `crates/norte-frontend/src/lib.rs` (`pub mod places;`)
- Modify: `crates/norte-frontend/src/layout/kinds.rs` (declare the kind)

The module owns rows, cursor and folding, and nothing else: it is handed
`&[Volume]` and `&[HotlistItem]`, so it is testable without a daemon. Same
shape as `help.rs`.

```rust
/// Una sección del sidebar. Dos en v1 y en este orden.
pub enum Section { Drives, Favorites }

/// Una fila pintable.
pub enum PlaceRow {
    Header { section: Section, folded: bool },
    Drive { label: Vec<u8>, mount: VPath, free: Option<u64>, total: Option<u64>, read_only: bool },
    Favorite { name: String, target: Result<VPath, String> },
}

pub struct PlacesState { /* privado */ }

impl PlacesState {
    pub fn new() -> Self;
    pub fn set_drives(&mut self, volumes: &[norte_proto::methods::Volume]);
    pub fn set_favorites(&mut self, items: &[(String, Result<VPath, String>)]);
    pub fn rows(&self) -> &[PlaceRow];
    pub fn cursor(&self) -> usize;
    pub fn up(&mut self);
    pub fn down(&mut self);
    pub fn toggle_fold(&mut self);
    /// A dónde ir, o `None` en una cabecera y en un favorito roto.
    pub fn activate(&self) -> Option<&VPath>;
}
```

`set_favorites` takes the pair rather than `HotlistItem` so `norte-frontend`
does not grow a dependency on `norte-config` for one struct; the TUI maps its
`HotlistItem` at the call site.

- [ ] **Step 1: write the failing tests**

```rust
/// Un favorito roto se PINTA, con su motivo. Un favorito que desaparece en
/// silencio es un fallo de config que no puedes ver.
#[test]
fn un_favorito_roto_sale_en_la_lista_y_no_navega() {
    let mut s = PlacesState::new();
    s.set_favorites(&[
        ("bueno".into(), Ok(VPath::parse("file:///casa").expect("wire"))),
        ("roto".into(), Err("err-invalid-path".into())),
    ]);
    let filas = s.rows();
    assert!(matches!(&filas[0], PlaceRow::Header { section: Section::Favorites, .. }));
    assert_eq!(filas.len(), 3);
    // el cursor sobre el roto no da destino
    s.down();
    s.down();
    assert!(s.activate().is_none());
}

/// `free_bytes` ausente NO es cero: es «no contestó». La regla vive en
/// `space.rs` y aquí se conserva el `Option` tal cual, sin sustituirlo.
#[test]
fn un_volumen_sin_espacio_conserva_el_none() {
    let mut s = PlacesState::new();
    s.set_drives(&[volumen_sin_espacio()]);
    let PlaceRow::Drive { free, total, .. } = &s.rows()[1] else {
        panic!("la fila 1 es el volumen");
    };
    assert!(free.is_none() && total.is_none());
}

/// La cabecera no navega, y plegar una sección esconde sus filas sin perder
/// el cursor en una fila que ya no existe.
#[test]
fn plegar_una_seccion_esconde_sus_filas_y_recoloca_el_cursor() {
    let mut s = PlacesState::new();
    s.set_drives(&[volumen("/", Some(1000), Some(4000))]);
    s.set_favorites(&[("casa".into(), Ok(VPath::parse("file:///casa").expect("wire")))]);
    assert_eq!(s.rows().len(), 4); // Drives, /, Favorites, casa
    s.toggle_fold();               // cursor en la cabecera Drives
    assert_eq!(s.rows().len(), 3);
    assert!(s.cursor() < s.rows().len());
    assert!(s.activate().is_none()); // sigue en una cabecera
}

/// La etiqueta de un volumen son BYTES (regla 1): un nombre no-UTF-8 no
/// revienta ni se pierde, se pinta con la conversión lossy explícita.
#[test]
fn una_etiqueta_no_utf8_sobrevive_como_bytes() {
    let mut s = PlacesState::new();
    s.set_drives(&[volumen_con_label(b"\xffdisco".to_vec())]);
    let PlaceRow::Drive { label, .. } = &s.rows()[1] else { panic!("volumen") };
    assert_eq!(label, b"\xffdisco");
}
```

- [ ] **Step 2: run and watch fail** — `just t norte-frontend`.
- [ ] **Step 3: implement `places.rs`** — rows are rebuilt from the two inputs
      plus the fold set; the cursor is clamped after every rebuild.
- [ ] **Step 4: declare the kind** in `KindRegistry::builtin()`:

```rust
// El sidebar: se enfoca y toma teclas, pero NO opta a ningún rol — un
// sidebar jamás es el destino de una copia. Uno y solo uno.
decl("places", (14, 5), true, true, false, SIN_ROLES),
```

with a test beside the existing ones asserting `roles.is_empty()` and
`!multi`.

- [ ] **Step 5: green** — `just t norte-frontend`, then
      `cargo doc -p norte-frontend --no-deps`.
- [ ] **Step 6: commit**

```bash
git add crates/norte-frontend/src/places.rs crates/norte-frontend/src/lib.rs crates/norte-frontend/src/layout/kinds.rs
git commit -m "feat(frontend): the state of a places sidebar, and its kind"
```

---

## Task 3: `TuiPanel::Places`, `KeyOwner`, and the toggle

**Files:**
- Modify: `crates/norte-tui/src/panel.rs` (variant + accessors)
- Modify: `crates/norte-tui/src/app.rs` (`KeyOwner`, `toggle_places`)
- Test: `crates/norte-tui/tests/places.rs` (new)

```rust
// panel.rs
pub enum TuiPanel {
    Browser(Box<Pane>),
    Places(Box<norte_frontend::places::PlacesState>),
    Unknown { kind: KindId, raw: Params },
}
```

`PaneSlots` gains `places(&self, id) -> Option<&PlacesState>`,
`places_mut`, `insert_places(id, state)` and `slot_of_kind(&Node, &str) ->
Option<SlotId>`. `as_browser`/`as_browser_mut` return `None` for the new
variant, which is what keeps `app.panes[i]` meaning *the i-th listing*.

```rust
// app.rs — quién se queda el teclado. No es el foco: `App.focus` sigue
// apuntando al listado en el que estabas, y toda operación sigue yendo ahí.
// El día que `focus` sea un SlotId (deuda de P6) esto se pliega dentro.
pub enum KeyOwner { Panes, Places, Preview }

impl App {
    /// Abre el sidebar (y le da el teclado), lo enfoca si estaba abierto sin
    /// teclado, o lo cierra si ya lo tenía.
    pub fn toggle_places(&mut self);
    pub fn key_owner(&self) -> KeyOwner;
    pub fn places_slot(&self) -> Option<SlotId>;
}
```

`toggle_places` mints a slot, `insert_places`, `self.layout =
self.layout.dock(self.focused_slot(), Edge::Left, Size::Fixed(16), &node)`,
then `panes.refresh_visible(&self.layout)`. Closing goes through
`close_slot` and restores `KeyOwner::Panes`.

- [ ] **Step 1: write the failing tests** in `tests/places.rs`

```rust
/// Abrir el sidebar no cambia cuántos LISTADOS hay ni cuál está enfocado. Es
/// la regla 7 del spec: el sidebar no es un lado.
#[test]
fn abrir_el_sidebar_no_toca_los_lados() {
    let mut app = app_de_prueba();
    let antes = (app.panes.len(), app.focus(), app.focused().dir().clone());
    app.toggle_places();
    assert_eq!(app.panes.len(), antes.0);
    assert_eq!(app.focus(), antes.1);
    assert_eq!(*app.focused().dir(), antes.2);
    assert!(app.places_slot().is_some());
    assert!(matches!(app.key_owner(), KeyOwner::Places));
}

/// Y cerrarlo deja el árbol EXACTAMENTE como estaba: sin Split degenerado
/// acumulándose una sesión entera.
#[test]
fn cerrar_el_sidebar_devuelve_el_arbol_de_antes() {
    let mut app = app_de_prueba();
    let antes = app.layout.clone();
    app.toggle_places();
    app.toggle_places();
    assert_eq!(app.layout, antes);
    assert!(app.places_slot().is_none());
    assert!(matches!(app.key_owner(), KeyOwner::Panes));
}

/// Segunda pulsación con el teclado en los panes: no cierra, ENFOCA. Cerrar
/// algo que el usuario acaba de mirar de reojo es la respuesta equivocada.
#[test]
fn con_el_sidebar_abierto_y_el_teclado_fuera_la_tecla_lo_enfoca() {
    let mut app = app_de_prueba();
    app.toggle_places();
    app.return_keys_to_panes();
    app.toggle_places();
    assert!(app.places_slot().is_some());
    assert!(matches!(app.key_owner(), KeyOwner::Places));
}
```

- [ ] **Step 2: run, watch fail** — `just t norte-tui`.
- [ ] **Step 3: implement** the variant, the accessors, `KeyOwner`,
      `toggle_places`, `return_keys_to_panes`.
- [ ] **Step 4: green** — `just t norte-tui` + `just c`.
- [ ] **Step 5: commit**

```bash
git add crates/norte-tui/src/panel.rs crates/norte-tui/src/app.rs crates/norte-tui/tests/places.rs
git commit -m "feat(tui): a places slot, and who owns the keyboard while it is up"
```

---

## Task 4: painting the sidebar

**Files:**
- Modify: `crates/norte-tui/src/ui.rs` (`draw_places`, called from `draw_body`)
- Test: `crates/norte-tui/tests/snapshots_ui.rs`, `tests/layout_anchor.rs`

In `draw_body`, after the browsers loop and before `draw_tasks`:

```rust
if let Some((id, rect)) = slot_of_kind(&res, &app.layout, "places") {
    if let Some(state) = app.panes.places(id) {
        draw_places(frame, rect, state, app.key_owner() == KeyOwner::Places, &app.theme);
    }
}
```

`slot_of_kind` is a sibling of `slot_rect` in `ui.rs`: first placed slot whose
kind matches. Rows: a header per section, `label mount free` for a drive with
`human_bytes` and `volumes-size-unknown` reused from the popup path, a name for
a favourite, greyed with its translated reason when it is `Err`.

- [ ] **Step 1: the acceptance test first**

```rust
/// El criterio de aceptación del plan, igual que en L1a y en P6: con el
/// sidebar CERRADO —el default— la pantalla ortodoxa es la misma celda por
/// celda. El usuario no nota nada hasta que lo abre.
#[test]
fn el_snapshot_ortodoxo_no_se_mueve_con_l3_dentro() {
    // mismo cuerpo que el snapshot 100x30 que ya existe; se compara contra el
    // .snap ya aceptado, sin regenerarlo.
}
```

Run it BEFORE writing `draw_places`. It must be green already; if it is not,
something earlier in the plan changed the default screen and that is the bug.

- [ ] **Step 2: write the failing snapshot test with the sidebar open**

100×30, sidebar open, one drive and two favourites. Then a **cell-level**
assertion, never `contains` — `TestBackend::to_string()` wraps every row in
quotes and a `contains` hid a one-cell offset for a whole batch in the menu
work:

```rust
let buffer = terminal.backend().buffer().clone();
// El sidebar ocupa las columnas 0..16 y el primer listado empieza en la 16.
let fila = row_texts(&buffer);
assert_eq!(fila[1].chars().nth(16), Some('│'), "el borde del primer listado cae en la 16");
```

- [ ] **Step 3: implement `draw_places`.**
- [ ] **Step 4: accept the snapshot.** `cargo-insta` is NOT installed here:
      accepting means deleting the `assertion_line:` header from the
      `.snap.new` and renaming it over the `.snap`.
- [ ] **Step 5: extend `layout_anchor.rs`** — with the sidebar open, the
      `Resolved` rectangle of every browser still matches the painted buffer.
      That file is the anchor for exactly this kind of drift.
- [ ] **Step 6: green** — `just t norte-tui`.
- [ ] **Step 7: commit, then the first gate run**

```bash
git add crates/norte-tui/src/ui.rs crates/norte-tui/tests/
git commit -m "feat(tui): the places sidebar on screen"
just ci-fast   # ONE run. Not to debug — to certify stages A and B.
```

---

## Task 5: sidebar keys, activation, and refreshing the drives

**Files:**
- Modify: `crates/norte-tui/src/main.rs` (key routing + the `host.volumes` fetch)
- Modify: `crates/norte-tui/src/app.rs` (`places_activate`)
- Test: `crates/norte-tui/tests/places.rs`

Keys while `KeyOwner::Places`: resolve under `Screen::Dialog`, which already
exists and which the presets already bind — `dialog.up`, `dialog.down`,
`dialog.confirm`, `dialog.cancel`. **No new keymap section and no new preset
rows.** `dialog.cancel` returns the keyboard to the panes; `dialog.confirm`
calls `places_activate`.

`places_activate` reads `PlacesState::activate()`; on `Some(path)` it changes
the directory **of the focused listing** through the same path `nav.enter`
uses, and hands the keyboard back to the panes. On `None` it does nothing.

Drives: `main.rs` calls `Backend::volumes` when `toggle_places` opened the
sidebar and when `pane.refresh` arrives with `KeyOwner::Places`, then
`places_mut(id).set_drives(&result.volumes)`. **Nowhere else, and no timer.**

- [ ] **Step 1: write the failing tests**

```rust
/// Enter sobre un favorito lleva al LISTADO ENFOCADO a ese sitio, y devuelve
/// el teclado. El sidebar no navega por su cuenta: es un mando, no un panel
/// con directorio propio.
#[test]
fn enter_en_un_favorito_lleva_al_listado_enfocado() {
    let mut app = app_de_prueba();
    app.toggle_places();
    poblar_favoritos(&mut app, &[("trabajo", "file:///trabajo")]);
    bajar_hasta_el_favorito(&mut app);
    let destino = app.places_activate().expect("un favorito da destino");
    assert_eq!(destino, VPath::parse("file:///trabajo").expect("wire"));
    assert!(matches!(app.key_owner(), KeyOwner::Panes));
}

/// Enter sobre una cabecera no navega y NO devuelve el teclado.
#[test]
fn enter_en_una_cabecera_no_hace_nada() {
    let mut app = app_de_prueba();
    app.toggle_places();
    assert!(app.places_activate().is_none());
    assert!(matches!(app.key_owner(), KeyOwner::Places));
}
```

- [ ] **Step 2: run, watch fail.**
- [ ] **Step 3: implement** `places_activate` in `app.rs` (returns
      `Option<VPath>` so the loop does the I/O) and the routing in `main.rs`.
- [ ] **Step 4: green** — `just t norte-tui`.
- [ ] **Step 5: commit**

```bash
git commit -am "feat(tui): the sidebar takes dialog keys and sends the listing somewhere"
```

---

## Task 6: the surface of `layout.places`

**Files:**
- Modify: `crates/norte-frontend/src/keymap/catalogue.rs`
- Modify: `crates/norte-tui/src/keymap.rs` (`"layout.places" => LayoutPlaces`)
- Modify: `crates/norte-tui/src/main.rs` (the `Command::LayoutPlaces` arm)
- Modify: `crates/norte-frontend/src/menu.rs` (the View menu)
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl`
- Modify: `crates/norte-help/tests/corpus.rs` (the command list)
- Modify: the seven `crates/norte-frontend/presets/keymap/*.toml`
- Test: `crates/norte-tui/tests/keymap.rs`

**Check `catalogue.rs` before writing the name.** `layout.places` is not there
today, neither `Live` nor `Planned` — confirm that again at implementation
time, because L1b built `tab.*`, found the catalogue had reserved `pane.tab-*`
as `planned` with two presets already binding it in grey, and had to rename
everything.

Strings: `help-cmd-layout-places` (a sentence) and `menu-item-layout-places`
(a **label**, capped short — the 74-column dropdown that covered both panels
was built from help sentences, and that is why the two key families exist).

Binding: `alt+b` in all seven presets. It is core — a surface you cannot open
without the palette is a surface nobody opens — and `alt+b` is free in all
seven; verify that with the rebind conflict check rather than by eye.

- [ ] **Step 1: the tests that already exist do most of this.** Run
      `just t norte-frontend` and `just t norte-tui` after the catalogue edit
      and read the failures: the catalogue tests demand a translated
      `help-cmd-*` in **both** locales, the menu tests demand nothing `Planned`
      and nothing in two menus, and `norte-help`'s corpus test demands the
      command be listed.
- [ ] **Step 2: add a test that the name is bound in every preset**

```rust
/// Un comando de núcleo atado en unos presets y no en otros es el agujero que
/// L1b metió con `pane.tab-next`: podías abrir una pestaña y no volver a ella
/// en cinco de los siete.
#[test]
fn layout_places_esta_atado_en_los_siete_presets() {
    use norte_frontend::keymap::{CATALOGUE, Effective, parse_keymap, presets};
    // Las secciones de `KeymapFile` son `pub(super)`: desde fuera del módulo
    // `keymap` se mira por el keymap EFECTIVO, que además es lo que el usuario
    // tiene de verdad (preset + capas + precedencia).
    let conocidos: Vec<&str> = CATALOGUE
        .iter()
        .map(|d| d.name)
        .collect();
    for nombre in presets::NAMES {
        let src = presets::source(nombre).expect("el preset existe");
        let kf = parse_keymap(src).expect("el preset parsea");
        let eff = Effective::build(&kf, None, &conocidos).expect("el preset fusiona");
        assert!(
            eff.bindings().iter().any(|(_, cmd)| *cmd == "layout.places"),
            "{nombre} no ata layout.places"
        );
    }
}
```

- [ ] **Step 3: implement** catalogue entry, `Command` variant, the arm
      (`Command::LayoutPlaces => app.toggle_places()`), the menu entry, both
      `.ftl` files, the corpus list, the seven presets.
- [ ] **Step 4: green** — `just t norte-frontend`, `just t norte-tui`,
      `just t norte-help`, `just c`.
- [ ] **Step 5: commit**

```bash
git commit -am "feat(tui,frontend): layout.places, in the catalogue, the menu and the seven presets"
```

---

## Task 7: one function that reads a file for a viewer

**Files:**
- Modify: `crates/norte-tui/src/main.rs` (extract out of `open_viewer`)

`open_viewer` (`main.rs:12332`) does two things in one future: read the
`VIEW_CAP` header, then try the styled plugin preview, the plain plugin
preview, and the raw view, in that order, with every failure degrading rather
than blocking. The modal wrapper around it — the `select!` that lets Esc
abandon — is what must NOT be shared.

Extract exactly the inner future:

```rust
/// Lee la cabecera de `path` y construye el `Viewer`, con la cadena de
/// preview de plugin y sus degradaciones. NO es cancelable por Esc: eso lo
/// pone quien la llama (`open_viewer` lo hace; el preview acoplado no puede,
/// porque nadie está esperando delante de él).
async fn viewer_for(backend: &Backend, path: &VPath) -> Result<Viewer, Error>;
```

`open_viewer` keeps its `select!` and calls it. **Behaviour identical**: this
task adds no test of its own beyond the ones already covering the viewer, and
if any of them moves, the extraction is wrong.

- [ ] **Step 1: extract.**
- [ ] **Step 2: `just t norte-tui`** — the existing viewer tests must be
      untouched and green.
- [ ] **Step 3: commit**

```bash
git commit -am "refactor(tui): the viewer's read, without the modal that wraps it"
```

---

## Task 8: the docked viewer and its per-slot fetch

**Files:**
- Create: `crates/norte-tui/src/preview.rs` (the decision, pure)
- Modify: `crates/norte-tui/src/panel.rs` (`TuiPanel::Viewer`)
- Modify: `crates/norte-tui/src/app.rs` (`toggle_preview`)
- Modify: `crates/norte-tui/src/main.rs` (`BySlot<PreviewFetch>`)
- Test: `crates/norte-tui/tests/preview.rs` (new)

The pure half, which is where every rule in the spec is pinned:

```rust
/// Qué debería estar enseñando el preview AHORA, o `None` si nada.
///
/// `None` cubre los tres casos del spec y por eso son testables sin daemon:
/// no hay hueco de preview colocado (cerrado, o detrás de una pestaña, o su
/// Split colapsó), el hueco al que sigue no existe, o el cursor está sobre un
/// directorio. Si esto devuelve `None`, `main.rs` no pide NADA: la suspensión
/// no es una comprobación aparte que alguien pueda olvidarse de escribir.
pub fn preview_target(app: &App, res: &Resolved) -> Option<(SlotId, VPath)>;
```

It resolves the follow with the engine's own `resolve_follow`, so a dead
followed slot degrades to `active` with a `FollowRetargeted` diagnostic —
existing behaviour that nothing exercised until now.

The I/O half in `main.rs`: `preview_fetch: BySlot<PreviewFetch>` polled in the
same rotating `poll_fn` scan as `decorate_fetch`. Each frame, if
`preview_target` differs from what that slot has loaded or has in flight,
**abort the in-flight one and start a new one**. A reply is applied only if the
slot still wants that exact path.

- [ ] **Step 1: write the failing tests**

```rust
/// Un preview detrás de una pestaña no pide NADA. La suspensión sale del
/// modelo (iterar los visibles), pero en L1b una fuga igual solo la vio un
/// test, así que aquí está el test.
#[test]
fn un_preview_oculto_no_produce_objetivo() {
    let mut app = app_de_prueba();
    app.toggle_preview();
    poner_cursor_en_un_fichero(&mut app);
    assert!(preview_target(&app, &resolver(&app)).is_some());
    esconder_el_preview_tras_una_pestana(&mut app);
    assert!(preview_target(&app, &resolver(&app)).is_none());
}

/// Un directorio bajo el cursor no se lee.
#[test]
fn un_directorio_bajo_el_cursor_no_produce_objetivo() {
    let mut app = app_de_prueba();
    app.toggle_preview();
    poner_cursor_en_un_directorio(&mut app);
    assert!(preview_target(&app, &resolver(&app)).is_none());
}

/// El objetivo va con su HUECO. Es la lección de P6 fase C: por posición, una
/// respuesta en vuelo se aplica a quien ocupe el sitio al llegar.
#[test]
fn el_objetivo_lleva_el_hueco_del_preview_no_su_posicion() {
    let mut app = app_de_prueba();
    app.toggle_preview();
    poner_cursor_en_un_fichero(&mut app);
    let (hueco, _) = preview_target(&app, &resolver(&app)).expect("hay objetivo");
    assert_eq!(Some(hueco), app.preview_slot());
}

/// El preview sigue al rol `active`: cambiar de listado cambia lo que enseña,
/// sin tocar el layout.
#[test]
fn cambiar_de_listado_cambia_el_objetivo() {
    let mut app = app_de_prueba_con_ficheros_distintos_en_cada_lado();
    app.toggle_preview();
    let a = preview_target(&app, &resolver(&app)).expect("objetivo").1;
    app.set_focus(1);
    let b = preview_target(&app, &resolver(&app)).expect("objetivo").1;
    assert_ne!(a, b);
}

/// Y si el hueco seguido MUERE, degrada al activo y lo DICE: el diagnóstico
/// existe desde L1a y hasta hoy no lo ejercitaba nadie.
#[test]
fn si_muere_el_hueco_seguido_el_preview_sigue_al_activo_y_lo_dice() {
    let mut app = app_de_prueba();
    app.toggle_preview();
    atar_el_preview_a_un_hueco_concreto(&mut app);
    cerrar_ese_hueco(&mut app);
    let res = resolver(&app);
    assert!(res.diagnostics.iter().any(|d| matches!(d, LayoutDiagnostic::FollowRetargeted { .. })));
    assert!(preview_target(&app, &res).is_some());
}
```

- [ ] **Step 2: run, watch fail.**
- [ ] **Step 3: implement** `preview.rs`, `TuiPanel::Viewer`, `toggle_preview`
      (docks `Edge::Right`, `Size::Weight(1)`, kind `viewer`, with
      `Bindings { follows: Some(Follow::Role(RoleId::Active)) }`), and the
      `BySlot<PreviewFetch>` plumbing in `main.rs`.
- [ ] **Step 4: green** — `just t norte-tui`, `just c`,
      `cargo doc -p norte-tui --no-deps`.
- [ ] **Step 5: commit, then the second and last `ci-fast`**

```bash
git add crates/norte-tui/
git commit -m "feat(tui): a viewer docked in a slot, following the active listing"
just ci-fast   # ONE run, certifying stages C and D.
```

---

## Task 9: painting the preview, and the two rules that make it safe

**Files:**
- Modify: `crates/norte-tui/src/ui.rs` (`draw_preview`)
- Modify: `crates/norte-tui/src/main.rs` (the denial path)
- Test: `crates/norte-tui/tests/preview.rs`, `tests/snapshots_ui.rs`

`draw_preview` reuses `draw_viewer`'s body inside a bordered block sized to the
slot instead of the whole frame. The status line (`viewer::status`) goes in the
block's bottom border: the reader must still know the encoding, and that line
is the only place that says it.

**The preview never asks.** A read denied by policy paints its reason inside
the slot. It must not enqueue the approval modal, and it must not clear what
was already there without saying why.

- [ ] **Step 1: write the failing tests**

```rust
/// Una denegación se PINTA, no se pregunta. El preview sigue al cursor, así
/// que un diálogo por pulsación convertiría bajar por un directorio en una
/// ráfaga de modales.
#[test]
fn una_lectura_denegada_pinta_el_motivo_y_no_abre_modal() {
    let mut app = app_de_prueba();
    app.toggle_preview();
    app.preview_failed(app.preview_slot().expect("hueco"), "reason-read-only");
    assert!(app.modal.is_none());
    let texto = pintar(&app, 100, 30);
    assert!(texto.contains(&norte_i18n::t_in(norte_i18n::Lang::Es, "reason-read-only")));
}
```

- [ ] **Step 2: run, watch fail.**
- [ ] **Step 3: implement** `draw_preview` and `App::preview_failed`.
- [ ] **Step 4: snapshot** at 100×30 with the preview open on a small text
      file. Cell-level geometry assertion, never `contains`. Accept it by hand
      (no `cargo-insta`).
- [ ] **Step 5: green + commit**

```bash
git commit -am "feat(tui): the docked preview on screen, and a denial that stays quiet"
```

---

## Task 10: the surface of `layout.preview`

**Files:** the same eleven files as task 6, plus
`crates/norte-tui/src/main.rs` for the key routing.

Same drill as task 6, and the same warning: check the catalogue for the name
before writing it. Keys while `KeyOwner::Preview` resolve under
`Screen::Viewer`, so `viewer.up`, `viewer.page-down`, `viewer.hex` and
`viewer.encoding` work in the docked one exactly as in the full-screen one, with
no new bindings at all. `viewer.close` returns the keyboard to the panes; it
does **not** close the slot — the slot is `layout.preview`'s to close.

Binding: `alt+q` in all seven presets, verified free by the conflict check.

- [ ] **Step 1: the preset-coverage test**, same shape as task 6's.
- [ ] **Step 2: implement** catalogue, `Command::LayoutPreview`, the arm, the
      View menu entry, both locales, the corpus list, the seven presets, the
      routing.
- [ ] **Step 3: green** — `just t norte-frontend`, `just t norte-tui`,
      `just t norte-help`, `just c`.
- [ ] **Step 4: commit**

```bash
git commit -am "feat(tui,frontend): layout.preview, bound and in the menu"
```

---

## Task 11: close the branch

**Files:** `CHANGELOG.md`, the memory files, this plan.

- [ ] **Step 1: the changelog entry**, in the house voice: what the reader can
      now do, and the one thing that is deliberately missing (no `metadata`
      panel, and why — its content is properties and recursive size, #139).
- [ ] **Step 2: reviewers, before the merge, dispatched from here.**
      `rust-reviewer` over the whole range, and `encoding-auditor` over tasks 2
      and 7 (drive labels are bytes; the preview inherits the viewer's encoding
      surface). No `protocol-guardian`: the wire never moved. Give each of them
      the commit range, what the change is for, and the specific doubt — a
      reviewer told only "review this diff" returns a checklist.
- [ ] **Step 3: apply BLOCKER and MAJOR findings in ONE pass**, and say which
      MINORs were skipped and why.
- [ ] **Step 4: `just ci`.** ONE run, in the foreground, never piped through
      `tail` — a killed pipe reports nothing at all. If it is red, reproduce
      the single failure with `just t <crate>` and fix it there; do not re-run
      the gate to check.
- [ ] **Step 5: write down what actually happened** — a *What actually
      happened* section in this file, like L1a, L1b and P6 have, and update
      `layout-huecos-pestanas.md` in memory.
- [ ] **Step 6: merge `--no-ff`.**

---

## Debt this plan creates on purpose

- **`KeyOwner` is a stand-in for `focus: SlotId`.** Three keyboard owners is
  the honest amount of state today; the day focus becomes a `SlotId` it folds
  in and `layout.focus-next` can cycle through the sidebar like anything else.
- **`layout.grow`/`shrink` on a `Fixed` slot** — check at task 3 whether
  `Node::resize` touches `Fixed` sizes at all. If it only moves weights, the
  sidebar cannot be widened and that is a one-line follow-up issue, not a
  reason to hold this plan.
- **No mouse on the sidebar.** Clicking a drive does nothing until someone adds
  the zones to `mouse.rs`, the way the tab strip got them.
- Still open from before: `metadata` (#139 first), remotes in the sidebar
  (#140 first), and P6's note about swapping ids in the tree instead of
  contents.
