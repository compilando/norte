# Fase 3 — Kind `panel` y `StyledFrame` (plan)

Spec: `docs/superpowers/specs/2026-09-15-historia-y-wow-design.md`, sección
«Fase 3». Rama `feat/kind-panel`. **Sube tres cosas a la vez y hay que decirlo
en el mismo commit**: paquete WIT nuevo `norte:panel@0.1.0`, protocolo
0.73.0 → 0.74.0 (`plugin.panel_render`, `PluginInfo.panels`) y el puente de la
ventana a 70 (`StyledFrame` cruzando). El wire del daemon SÍ se toca, así que
`protocol-guardian` es obligatorio antes de commitear.

Gate: `just t <crate>` en el bucle; `just ci-fast` + `just gui-ci` cada ~3
tareas; `just ci` una vez antes de fusionar.

## Lo que el mapeo dejó claro

- **`KindRegistry` está abierto en el TIPO y cerrado en el CABLEADO.** Su
  rustdoc promete «más adelante, que un plugin aporte un kind», y `insert`
  existe — pero `KindRegistry::builtin()` se construye en **catorce sitios**
  (`norte-tui/src/app.rs`, `app/focus.rs`, `app/layout.rs`, `ui/pickers.rs`,
  `norte-ui-host/src/controller/mod.rs`, y siete usos en `norte-frontend`
  entre `layout_picker`, `panelbar`, `roles` y `presets`). Un kind de plugin
  que no llegue a TODOS ellos existe en el árbol y no se pinta, no se enfoca o
  no sale en el selector de disposiciones. Esta es la tarea grande de la fase,
  y no la de WIT.
- **El punto de enganche del pintado ya existe y tiene nombre**:
  `SlotView::Unsupported { kind_name, kind_name_hostile }` (`views.rs:101`) es
  lo que hoy sale para un kind que el frontend no conoce. Un panel de plugin NO
  es eso: es un kind conocido cuyo contenido lo fabrica un guest. Variante
  nueva `SlotView::Panel`, y `Unsupported` se queda para lo que de verdad no se
  conoce.
- **Un kind nuevo es un PAQUETE WIT, no un bump** (ADR 0094: el host sirve UNA
  versión de cada paquete, así que subir `norte:plugin` invalidaría los ocho
  guests instalados). `wit/deps/panel/panel.wit` + una línea en `SERVED_WIT` +
  `tests/wit_packages.rs`. El molde exacto es `wit/deps/thumbnail`, cuyo
  encabezado ya explica por qué va aparte.
- **El camino de una RPC de plugin está hecho**: `HostBackend` declara
  `plugin_thumbnail`/`plugin_decorate` como fail-soft («cosmético por
  contrato»), y el panel es igual — un plugin que no contesta deja la última
  foto puesta, nunca tumba la pantalla.
- **`StyledSpan` (`norte-frontend/src/ansi.rs`) ya es el ladrillo** y ya cruza
  el puente como `SpanView` con rol validado contra el conjunto cerrado de
  `norte_theme::Role`. `StyledFrame` no inventa estilo: reusa eso.

## T1 — Compartido: `StyledFrame` (`norte-frontend`)

1. `frame::StyledFrame { lines: Vec<Vec<StyledSpan>>, hits: Vec<Hit> }` y
   `Hit { row, col, width, command: String, arg: Option<String> }`.
2. `StyledFrame::hit_at(row, col) -> Option<&Hit>`, con test de solapes y de
   fila/columna fuera de rango.
3. Cotas al construir desde el wire: 256 líneas, 256 spans por línea, 128
   hits. Recorte silencioso y contado, como el resto de `clamp_display`.
4. Proptest: un frame recortado nunca deja un `Hit` apuntando a una fila que
   ya no está.

## T2 — Protocolo (0.74.0) y WIT (`norte:panel@0.1.0`)

1. `wit/deps/panel/panel.wit`: `render(kind, context{cols, rows, lang,
   cursor-name}, state: list<u8>, event: none | click(row,col) | key(command))
   -> result<frame{lines, hits, state}, string>`. Sin teclas crudas: el guest
   recibe COMANDOS, que es lo que mantiene la policy intacta.
2. `SERVED_WIT` + `tests/wit_packages.rs` + `norte-cli/src/doctor.rs`. Ojo al
   truco de la memoria: los tres reescriben `@X` por `@Y` en bytes, así que la
   versión nueva debe tener la MISMA longitud.
3. Proto: `plugin.panel_render` (cotas 4 MiB de respuesta, `state` 64 KiB) y
   `PluginInfo.panels: Vec<PluginPanelInfo>`. Golden de tipos, schema y
   `norte-cli` (`NORTE_UPDATE_GOLDEN=1`).
4. Manifiesto: `[[contributions.panel]] kind, title, min-cols, min-rows` en
   `Contributions`. El `KindId` es `plugin:<id>:<kind>` y no puede chocar con
   un built-in: test que lo afirme.
5. **`protocol-guardian` aquí**, antes de seguir.

## T3 — El registro de kinds, de verdad abierto

1. `KindRegistry::insert_panels(&[PluginInfo])` en `norte-frontend`, y cada
   frontend con SU fuente de datos: la TUI una sonda (`spawn_panels`) drenada
   por el bucle, el host un `Fondo::PanelesDePlugin` pedido en `start`. Esa
   fuente era el trabajo de verdad, no el cableado.
2. Los llamantes del registro, revisados. **Eran cuatro en producción, no
   catorce** — la cuenta original venía de grepear `builtin()` incluyendo
   tests y el propio `norte-frontend`:
   - `focus_stop` (TUI) pasaba por `builtin()`, donde un kind aportado no
     existe: quedaba fuera del anillo de `Tab` y fuera del alcance del ratón.
   - `draw_layout_picker` / `draw_layout_preview` (TUI) reciben el registro
     vivo desde `ui.rs`.
   - `kind_con_teclado` (TUI) devuelve `Option<&str>`: de un panel aportado no
     puede contestar una constante.
   - `panel_slot` / `panel_kind` (TUI) resuelven por PREFIJO `plugin:`, porque
     `plugin:<id>:<kind>` no se conoce al compilar.

   Dos cosas que el plan daba por pendientes y no lo estaban: la barra de
   paneles (`panelbar`) ya se derivaba de lo declarado, y el host no tiene
   barrera de foco que abrir. Lo que sí hizo falta y no estaba escrito:
   `KeyOwner::Panel` en la TUI, sin payload — llevar dentro el `SlotId`
   rompería las 86 comparaciones por `==`, y `multi: false` garantiza que hay
   como mucho uno visible.
3. Test que recorra ese camino: un kind de plugin declarado se encuentra, toma
   el teclado y aparece en el selector.

## T4 — Pintado en los dos frontends

1. TUI: `draw_panel(frame, area, theme)` sobre `StyledFrame`; clic dentro del
   área resuelve `hit_at` y despacha el comando por el camino normal.
2. Ventana: `SlotView::Panel` (puente 70) y `paintPanel` en `render/panel.ts`,
   con los `Hit` como zonas pulsables. Un `Hit` no ejecuta nada por su cuenta:
   manda la acción que ya existe.
3. Cuándo se repinta: al cambiar directorio o cursor del panel con foco (con
   coalición), al redimensionar, y tras un evento. Un plugin lento **no
   bloquea**: se queda la última foto con indicador.
4. Test de paridad: el mismo `StyledFrame` pintado en los dos, mismas filas.

## T5 — Demo oficial `plugins/git-panel`

Rama (`.git/HEAD`), commit (`.git/logs/HEAD`, última línea) y los diez últimos
movimientos del reflog, con `location-root-marker = ".git"`. Con `git-status`
(columna) cierra la mejora 7 del programa.

## T6 — Ayuda, ADR y cierre

- Topic de ayuda (en/es) del kind `panel`: qué es, cómo se abre, y que lo que
  pinta es de un plugin.
- ADR 0116: por qué un paquete WIT propio, por qué el guest recibe comandos y
  no teclas, por qué el estado es opaco y acotado, y por qué un panel lento se
  degrada en vez de bloquear.
- CHANGELOG, memoria, `just link` y `just link-gui`.
- Revisores antes de commitear: `protocol-guardian` (obligatorio),
  `security-reviewer` (un guest pinta y pide comandos) y `rust-reviewer`.
