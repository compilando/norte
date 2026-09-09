# Árbol que sigue al panel, Tab de n listados y scroll del visor

> **For agentic workers:** REQUIRED SUB-SKILL: `superpowers:executing-plans`.
> Los pasos llevan checkbox (`- [ ]`).

**Goal:** tres huecos de paridad que un lector encuentra en la primera hora:
el árbol no sigue al panel que navega, `Tab` deja de rotar en cuanto hay tres
listados, y el visor no puede llegar a la derecha de una línea larga.

**Architecture:** los tres se arreglan en el modelo COMPARTIDO
(`norte-frontend`) y se cablean en el embudo que cada frontend ya tiene
—`aterrizar_listado` en `norte-ui-host`, `settle_cd` en `norte-tui`—, no en
cada call site. Ninguno toca el wire; el del visor sube el puente.

**Tech Stack:** Rust, ratatui (TUI), TypeScript + Tauri (ventana).

**Spec:** este fichero. Los tres son bugs reportados a mano el 2026-09-09;
no hay spec previa. Las decisiones están en la **ADR 0100**.

**Estado: ejecutado el 2026-09-09**, en siete commits sobre
`chore/arbol-tab-y-scroll-del-visor`. Lo que salió distinto del plan:

- `viewer.line-home` se **descartó**: `home`/`end` ya son `viewer.top`/
  `viewer.bottom` en los siete presets, y un comando sin chord libre en
  ninguno es exactamente el fallo que la regla de los siete presets existe
  para evitar. `viewer.left` con contador cubre volver al principio.
- La barra HORIZONTAL solo va en el visor a pantalla completa. El acoplado
  lleva la línea de estado en su borde de abajo, que es el único sitio donde
  ese hueco dice QUÉ se está leyendo; taparla cambiaría un dato por una
  insinuación.
- Hizo falta una acción de puente que el plan no previó: **`viewer_scroll`**.
  Una rueda no es una tecla, y fabricar pulsaciones de flecha para expresarla
  ataba el gesto a que nadie reatara esas flechas.
- El árbol también sigue al **cambio de foco**, no solo a la navegación: es
  el otro momento en que cambia dónde mira el panel activo.
- Dos listas escritas a mano que el plan no nombraba y el gate sí: el
  conjunto `counts` del catálogo (ADR 0044) y `DOCUMENTED` en
  `norte-help/tests/corpus.rs`.
- Un test intermitente aparecido bajo carga —
  `con_un_plan_en_vuelo_escape_cancela_el_filtro` — se diagnosticó y arregló
  en la misma rama: esperaba un parche que un `siguiente_foto` anterior se
  había tragado. Ahora pregunta por la foto, que es idempotente.

## Global Constraints

- `pane.switch` cicla **solo listados**, n-way. `layout.focus-next` sigue
  siendo el anillo completo (listados + laterales). Decisión del 2026-09-09.
- El árbol **no se re-ancla** al navegar dentro de su raíz: revela la rama y
  conserva lo abierto. Re-anclar solo cuando el destino no cuelga de la raíz.
- Teclas nuevas: **los siete presets** (`orthodox`, `vim`, `cua`, `krusader`,
  `far`, `norton`, `total-commander`), catálogo, `help-cmd-*` en `en` **y**
  `es`, y golden de `norte-cli` (`NORTE_UPDATE_GOLDEN=1`).
- `BRIDGE_VERSION` sube 58 → 59 al añadir campos a `ViewerView`; `types.ts`
  va con ella.
- Nombres como bytes: el recorte horizontal se hace por CELDAS de pantalla
  (`norte_frontend::cells`), nunca por bytes ni por `char`.

---

### Task 1: `Tree::follow` — revelar sin tirar lo abierto

**Files:**
- Modify: `crates/norte-frontend/src/tree.rs`
- Test: mismo fichero, `mod tests`

**Interfaces:**
- Produce: `Tree::follow(&mut self, dir: &VPath)`, `Tree::revealing(&self) -> Option<&VPath>`.
- `follow` decide: si `dir` cuelga de la raíz → expande la cadena de
  ancestros y anota `dir` como pendiente; si no → `anchor(dir)`.
- `insert_children` liquida el pendiente cuando la fila por fin existe
  (poner el cursor ahí y olvidarlo).

- [ ] Test rojo: `follow` a un nieto expande al padre, deja el cursor en el
      nieto y **conserva** una rama hermana abierta.
- [ ] Test rojo: `follow` fuera de la raíz re-ancla (filas = 1).
- [ ] Test rojo: `follow` a un nieto todavía NO listado no mueve el cursor
      todavía, `wants()` va pidiendo la cadena, y al llegar el último
      `insert_children` el cursor aterriza en el nieto.
- [ ] Test rojo: `follow` a la raíz misma deja el cursor en 0 y no tira nada.
- [ ] Implementar `revealing: Option<VPath>` en el struct, `follow`, y el
      cierre en `insert_children`.
- [ ] `just t norte-frontend`
- [ ] Commit.

### Task 2: cablear el árbol en los dos embudos

**Files:**
- Modify: `crates/norte-ui-host/src/controller/listing.rs` (`aterrizar_listado`)
- Modify: `crates/norte-ui-host/src/controller/tree.rs` (helper `seguir_ramas`)
- Modify: `crates/norte-tui/src/navigate.rs` (`settle_cd`)
- Test: `crates/norte-ui-host/tests/`, y el test de árbol del TUI

**Interfaces:**
- Consume: `Tree::follow` de la Task 1.
- Produce: `Estado::seguir_ramas(&mut self, slot: u32, backend, buzon)` —
  no hace nada si no hay hueco de árbol o si `slot` no es el activo.

- [ ] Test rojo (ui-host): con el árbol abierto, navegar el panel activo a
      un subdirectorio mueve el cursor del árbol a esa rama y **sube**
      `generation`.
- [ ] Test rojo (ui-host): navegar el panel NO activo no mueve el árbol.
- [ ] Test rojo (TUI): igual, sobre `settle_cd`.
- [ ] Implementar `seguir_ramas` + la llamada en `aterrizar_listado` (junto a
      `pedir_capacidades`/`sondear`/`adornar`) y en `settle_cd`.
- [ ] `just t norte-ui-host` y `just t norte-tui`
- [ ] Commit.

### Task 3: `pane.switch` cicla los listados, n-way

**Files:**
- Modify: `crates/norte-tui/src/app/focus.rs` (`switch_focus`)
- Modify: `crates/norte-ui-host/src/commands.rs` (`pane.switch` deja de
  compartir brazo con `layout.focus-next`)
- Modify: `crates/norte-ui-host/src/controller/panel.rs` (foco solo-listados)
- Test: `focus.rs`, y el test de paridad que ya cubre los comandos

**Interfaces:**
- Produce (TUI): `App::switch_focus` cicla `0..panes.len()`.
- Produce (host): `Efecto::Foco { atras, solo_listados: bool }`.

- [ ] Test rojo (TUI): con tres listados visibles, tres `switch_focus`
      vuelven al de partida y el tercero es alcanzable.
- [ ] Test rojo (TUI): con un solo listado, `switch_focus` es no-op.
- [ ] Test rojo (host): con árbol abierto y tres listados, `pane.switch`
      recorre los tres y NO para en el árbol; `layout.focus-next` sí para.
- [ ] Implementar los dos.
- [ ] `just t norte-tui` y `just t norte-ui-host`
- [ ] Commit.

### Task 4: el visor scrollea a lo ancho

**Files:**
- Modify: `crates/norte-frontend/src/display.rs` (`skip_cells`)
- Modify: `crates/norte-frontend/src/viewer.rs` (`hscroll`, `max_cols`,
  `scroll_left/right/home`, recorte en `rows` y en `plugin_styled_rows`)
- Modify: `crates/norte-frontend/src/keymap/catalogue.rs`
- Modify: los siete `crates/norte-frontend/presets/keymap/*.toml`
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl`
- Modify: `crates/norte-tui/src/keymap.rs`, `crates/norte-tui/src/dispatch.rs`
- Modify: `crates/norte-ui-host/src/commands.rs`,
  `crates/norte-ui-host/src/controller/viewer.rs`
- Test: `viewer.rs`, `display.rs`, golden de `norte-cli`

**Interfaces:**
- Produce: `display::skip_cells(&str, usize) -> &str` (jamás parte un carácter
  ancho: si el corte cae en medio, el carácter entero se va).
- Produce: `Viewer::hscroll() -> usize`, `Viewer::max_cols() -> usize`,
  `scroll_left(n)`, `scroll_right(n)`, `scroll_home()`.
- Comandos nuevos: `viewer.left`, `viewer.right` (los dos con cuenta).

- [ ] Test rojo: `skip_cells` sobre ASCII, sobre CJK (corte a mitad de un
      carácter de dos celdas) y sobre combinantes.
- [ ] Test rojo: `rows()` con `hscroll` recorta por la izquierda; el tope es
      `max_cols`; el hexview NO scrollea a lo ancho.
- [ ] Test rojo: `toggle_hex` y un encoding nuevo ponen `hscroll` a 0.
- [ ] Implementar el modelo.
- [ ] Atar los comandos: catálogo, TUI (`keymap`+`dispatch`), host
      (`commands`+`controller/viewer`), los siete presets a `left`/`right`,
      `help-cmd-viewer-left`/`-right` en `en` y `es`.
- [ ] `just t norte-frontend`; `NORTE_UPDATE_GOLDEN=1 just t norte-cli`
- [ ] Commit.

### Task 5: que se VEA que hay más — barras y rueda

**Files:**
- Modify: `crates/norte-tui/src/ui/panels.rs` (`draw_viewer`, `draw_preview`)
- Modify: `crates/norte-tui/src/ui/help.rs` (`render_scrollbar` con
  orientación)
- Modify: `crates/norte-tui/src/mouse.rs` (la rueda llega al visor)
- Modify: `crates/norte-ui-host/src/dto.rs` (`ViewerView.first_col`,
  `.total_cols`)
- Modify: `crates/norte-ui-host/src/bridge.rs` (58 → 59)
- Modify: `crates/norte-gui-tauri/ui/src/types.ts`, `render.ts`, `style.css`
- Test: `panels.rs` (foto de terminal), `dto`/puente, y el test de paridad

- [ ] Test rojo (TUI): el visor sobre un fichero más alto que la ventana
      pinta barra vertical; con líneas más anchas, horizontal; y con ambas
      cabiendo, ninguna.
- [ ] Test rojo (TUI): la rueda sobre el visor mueve `scroll`, no el listado.
- [ ] Test rojo (host): `ViewerView` lleva `first_col`/`total_cols` y el
      puente es 59.
- [ ] Implementar TUI, DTO y renderer (rueda en el visor de la ventana,
      barras con las mismas dos preguntas).
- [ ] `just t norte-tui`, `just t norte-ui-host`, y el build del webview.
- [ ] Commit.

### Task 6: cerrar

- [ ] ADR: `/adr` — «el árbol sigue al panel revelando, no re-anclando», y
      «`pane.switch` es el anillo de listados; `layout.focus-next` el de la
      pantalla».
- [ ] `CHANGELOG.md`.
- [ ] Memoria del proyecto.
- [ ] `just ci-fast` (UNA vez), y `just ci` antes de la fusión (UNA vez).
