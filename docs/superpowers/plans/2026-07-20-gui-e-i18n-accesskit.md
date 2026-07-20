# GUI-e i18n + AccessKit foundational — plan (cierra el MVP GUI, M5 hito 2)

> **For agentic workers:** subagent-driven o executing-plans. Diseño APROBADO por oscar (scope: i18n completo + AccessKit foundational). Sin spec aparte (scope confirmado); este doc es plan + diseño.

**Goal:** la GUI deja de hardcodear ES (strings → Fluent vía `norte-i18n`, como la TUI) y expone un árbol AccessKit foundational (roles/labels en panes/filas/viewer/modales), cerrando el MVP de la GUI.

**Architecture:** i18n = cargar el locale al arranque (`norte_i18n::force(Lang::from_env())`) + reemplazar los strings ES de `main.rs` por `t("gui-…")`/`ta(...)` con claves nuevas en `es.ftl`+`en.ftl` (y reusar las claves `viewer-*`/`eol-*` que ya existen para unificar `viewer_status`). AccessKit = `div().role(accesskit::Role::…)` en los elementos clave + `a11y_synthetic_children` donde haga falta, gateado por `window.is_a11y_active()`, verificado con `window.debug_a11y_tree_json()`. Rama `m5-cierre` (junto al resto del cierre).

**Datos verificados (2026-07-20):**
- `norte_i18n`: `Lang{from_env, negotiate}`, `force(Lang)->bool`, `t(id)->String`, `ta(id, &[(&str,&str)])->String`, `t_in`/`ta_in`. Bundles `crates/norte-i18n/i18n/{es,en}.ftl` (COMPARTIDOS TUI+GUI). La TUI carga en `main.rs:179-185` (`Lang::from_env` → `force`). Hay tests de paridad en/es (toda clave en ambos locales).
- Claves Fluent YA existentes reusables por el viewer_status GUI: `viewer-binary`, `viewer-forced`, `viewer-lossy`, `viewer-truncated`, `eol-mixed`, `eol-none` (las usa `norte_tui::viewer::status`).
- GPUI a11y (rev f14fea9): `div().role(accesskit::Role)`, `aria_toggled`/`aria_orientation`, `on_a11y_action`, `a11y_synthetic_children(|builder: &mut A11ySubtreeBuilder| …)`; `Window::{is_a11y_active()->bool, debug_a11y_tree_json()->Option<String>, on_a11y_action(...)}`; el árbol se emite por-frame (`a11y.end_frame`). Ejemplo idiomático: `crates/gpui/examples/a11y.rs` (leerlo). `accesskit` = dep del workspace (gpui la usa; la GUI la añade a su Cargo.toml).
- GUI strings ES hardcodeados (en `crates/norte-gui/src/main.rs` salvo nota): modales (`modal_lines`/`render_modal`: "Copiar/Mover N elemento(s) →", "Borrar N elemento(s)", "PAPELERA"/"PERMANENTE", "Conflicto: …", "… y N más", pies "y confirmar / n Esc cancelar / p alternar permanente / o sobrescribir / s saltar / c cancelar"); banners ("operación rechazada: {e}", "visor: {e}", "keymap: {e}", "config inválida: {e}", "error: {e}"); estados ("cargando…", "abriendo visor…", "(sin tasks)", "(directorio vacío)"); task strip (`task_line`: "copy/move/delete/undo/search/task", "pending/running/paused/done/cancelled/failed"); viewer_status ("binario", "(forzado)", "LF/CRLF/CR", "EOL mixto", "sin EOL", "con pérdidas", "truncado"); cabecera viewer ("via {plugin}"). HOSTILE_BADGE/MARK_MARKER son símbolos (no i18n).
- La GUI tiene `rustfmt.toml` (style_edition=2024) → `cargo fmt` determinista.

---

### Task 1: i18n — cargar locale + extraer strings ES a Fluent

**Files:** Modify `crates/norte-gui/src/main.rs`, `crates/norte-gui/Cargo.toml` (+`norte-i18n`), `crates/norte-i18n/i18n/es.ftl`, `crates/norte-i18n/i18n/en.ftl`.

- [ ] **Step 1: dep + carga** — `Cargo.toml`: `norte-i18n = { path = "../norte-i18n" }` (mira cómo lo declara norte-tui). En `main()` (antes de abrir la ventana): `let lang = norte_i18n::Lang::from_env(); let _ = norte_i18n::force(lang);` (patrón TUI main.rs:179-185; sin flag CLI, solo env).
- [ ] **Step 2: claves Fluent** — añade a `es.ftl` Y `en.ftl` (paridad OBLIGATORIA — hay test) las claves `gui-*` para TODOS los strings del inventario de arriba. Convención: `gui-modal-copy = Copiar { $n } elemento(s) →`, `gui-modal-permanent = PERMANENTE`, `gui-loading = cargando…`, `gui-viewer-opening = abriendo visor…`, `gui-tasks-empty = (sin tasks)`, `gui-dir-empty = (directorio vacío)`, `gui-banner-op-rejected = operación rechazada: { $err }`, `gui-task-kind-copy = copy`, `gui-task-state-running = running`, `gui-viewer-via = via { $plugin }`, etc. Para el `viewer_status` REUSA las claves existentes `viewer-binary`/`viewer-forced`/`viewer-lossy`/`viewer-truncated`/`eol-mixed`/`eol-none` (LF/CRLF/CR son literales técnicos, NO i18n). Los args (`$n`, `$err`, `$plugin`) van por `ta(id, &[("n", &n.to_string()), …])`.
- [ ] **Step 3: reemplaza los strings** — en `main.rs`, sustituye cada string ES hardcodeado por `norte_i18n::t("gui-…")` o `norte_i18n::ta("gui-…", &[…])`. `viewer_status` pasa a usar `t("viewer-binary")`/`t("viewer-forced")`/etc. (unifica con la TUI, cierra la desviación de GUI-d). OJO orden de saneado: los nombres de archivo/rutas siguen por `display_name`/`path_display` (NO son i18n — bytes); solo las ETIQUETAS de UI van por Fluent. Un `ta` con un `$err` que contenga el `Display` de un `norte_proto::Error` es seguro (taxonomía categórica, sin bytes crudos — ya auditado en GUI-b).
- [ ] **Step 4: test de paridad + verde** — corre el test de norte-i18n que exige toda clave en ambos locales (`cargo nextest run -p norte-i18n`); si falla por una `gui-*` en un solo locale, añádela al otro. `cd crates/norte-gui && cargo build -p norte-gui && cargo nextest run --bin norte-gui && cargo clippy --bin norte-gui --all-targets -- -D warnings && cargo fmt`. Desde la raíz `just ci` (toca norte-i18n, crate del workspace). Añade un test GUI `gui_modal/viewer_status_localizado` que fije el locale (`norte_i18n::force(Lang::Es)`) y verifique una cadena localizada + que sigue sin hazards crudos con nombre hostil.
- [ ] **Step 5: Commit** — `feat(gui): i18n — strings de la GUI por Fluent (GUI-e T1)`

---

### Task 2: AccessKit foundational — árbol de a11y

**Files:** Modify `crates/norte-gui/src/main.rs`, `crates/norte-gui/Cargo.toml` (+`accesskit`).

Lee PRIMERO `crates/gpui/examples/a11y.rs` (idioma real del rev) y `crates/gpui/src/window/a11y.rs`.

- [ ] **Step 1: dep** — `Cargo.toml`: `accesskit = "…"` (la MISMA versión que usa gpui — mírala en el `Cargo.toml` de gpui o en `cargo tree`); o usa el re-export si gpui expone `gpui::accesskit`. Verifícalo antes de añadir dep nueva.
- [ ] **Step 2: roles en los elementos clave** — en `render`/`render_pane`/`render_row`/`render_viewer`/`render_modal`/`render_task_strip`, añade `.role(...)` (todo gateable por `window.is_a11y_active()` para no pagar coste sin lector):
  - Root: `Role::Window` (o el que use el ejemplo).
  - Cada pane: `Role::List` (o `Tree`) + label «pane izquierdo/derecho» (i18n `gui-a11y-pane-left/right`); el pane con foco marca `aria`/estado activo.
  - Cada fila (`render_row`): `Role::ListItem` con el nombre accesible = `display_name` SANEADO (el label a11y también se sanea — un lector no debe leer bidi/controles crudos; reusa el `label` ya saneado de `row_label`). La fila seleccionada marca estado `selected`; la marcada, `aria_toggled`.
  - Viewer (`render_viewer`): `Role::Document`; el status como label/description.
  - Modal (`render_modal`): `Role::Dialog` + el texto del modal (líneas de `modal_lines`, ya saneadas) como contenido accesible.
  - Task strip: `Role::List` con filas `Role::ListItem` (`task_line`).
  - Si un role simple no basta (nombre accesible), usa `a11y_synthetic_children` para construir nodos con `set_label` (ver ejemplo). El label SIEMPRE del texto YA saneado (nunca bytes crudos — regla 1 aplica también al árbol a11y).
- [ ] **Step 3: verificación estructural** — sin lector de pantalla: un modo debug que vuelque `window.debug_a11y_tree_json()` bajo `NORTE_GUI_DEBUG` (o una tecla) para confirmar que el árbol se construye con los roles/labels esperados. Documenta que la prueba con AT-SPI/lector real queda pendiente de oscar (Linux). Si `is_a11y_active()` es false sin lector, fuerza la construcción bajo `NORTE_GUI_DEBUG` para poder volcar el JSON.
- [ ] **Step 4: verde** — `cd crates/norte-gui && cargo build -p norte-gui && cargo nextest run --bin norte-gui && cargo clippy --bin norte-gui --all-targets -- -D warnings && cargo fmt`. (Sin tests de render por política spike; la verificación del árbol es el volcado JSON manual.)
- [ ] **Step 5: Commit** — `feat(gui): AccessKit foundational — árbol de a11y con roles/labels (GUI-e T2)`

---

### Task 3: cierre GUI-e + MVP — reviewers + gate + merge del cierre M5

- [ ] **Step 1: reviewers**:
  - **encoding-auditor** sobre los labels a11y + los strings i18n: los nombres de archivo/rutas en labels a11y van SANEADOS (`display_name`/`path_display`), jamás bytes crudos a un lector de pantalla; los `ta` con `$err`/`$plugin` no filtran contenido crudo. Confirmar que el árbol a11y no reintroduce bidi/controles (mismo criterio que el render visual).
  - **rust-reviewer** sobre GUI-e (i18n + a11y): reglas duras, la carga de locale, el gateo `is_a11y_active`, sin unwrap fuera de invariante.
  - Aplicar hallazgos.
- [ ] **Step 2: gate** — `just ci` EXIT=0 (norte-i18n con las claves nuevas + paridad en/es). norte-gui aparte: `cargo build/clippy/nextest -p norte-gui`.
- [ ] **Step 3: doc de cierre** — nota corta `docs/notes/cierre-m5-mvp-gui.md`: M5 hito 2 (MVP GUI) COMPLETO (GUI-a..e); criterio de salida M5 «GUI y TUI sobre la misma sesión simultáneamente» (demostrado); deuda GUI viva (#83 nav-franja parcial, AccessKit verificación AT-SPI pendiente, imágenes en viewer, hot-reload keymap, edición 2024 completa).
- [ ] **Step 4: issues de deuda** — abre las que queden (AccessKit verificación AT-SPI/lector; edición 2024 completa con migración RPIT de los closures #87; task-strip nav).
- [ ] **Step 5: merge del cierre M5** — la rama `m5-cierre` lleva #86/#88 + deuda #83/84/85 + GUI-e. `just ci` verde + norte-gui verde → merge `--ff-only` a main (o presenta a oscar para verificación interactiva antes). Commit de cierre + push.

---

## Self-review del plan (hecho)
- Cobertura: i18n completo (carga locale + inventario de strings → Fluent, reusa claves viewer-*/eol-*) (T1); AccessKit foundational (roles/labels en panes/filas/viewer/modales/strip, gateado, verificado por JSON) (T2); reviewers (encoding OBLIGATORIO por los labels a11y = superficie de texto de un tercero/nombre) + gate + cierre M5 (T3). Fuera de alcance: verificación AT-SPI real (pendiente oscar), edición 2024 completa, imágenes viewer.
- Riesgo: los labels a11y son NUEVA superficie de display de nombres → encoding-auditor OBLIGATORIO (T3) para que no cuelen bytes crudos a un lector. La API a11y es de bajo nivel → T2 lee el ejemplo del rev primero. i18n toca norte-i18n (gate) → paridad en/es exigida por su test.
- Orden: T1 (i18n) y T2 (a11y) ambos tocan main.rs → SECUENCIALES. Van sobre el commit de deuda 58907d6 ya en la rama.
