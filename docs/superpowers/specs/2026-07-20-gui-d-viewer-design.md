# GUI-d — viewer F3 (M5 hito 2, sub-proyecto 4) — diseño

- Fecha: 2026-07-20
- Estado: **IMPLEMENTADO** (T1–T4, cerrado 2026-07-20; commits 9b38fe8..ce8e0b8,
  rama `gui-d-viewer`). `just ci` EXIT=0; norte-frontend + norte-tui + norte-gui
  30 tests verdes + clippy limpio. Reviewers rust (por task) + encoding
  aplicados. **Verificación GUI interactiva PENDIENTE de oscar** (headless).
  Desviaciones (deuda anotada):
  - **encoding ALTA (H1) corregida**: `render_line` del viewer de texto
    enmascaraba solo `is_control()` (Cc), dejando pasar bidi (U+202E)/invisibles
    (ZWSP)/Zl-Zp/TAG crudos → Trojan Source (CVE-2021-42574), y GPUI reordena
    bidi. Cambiado a `is_terminal_hazard` (misma política que los nombres);
    arregla AMBOS frontends. Test + fixture bidi. Era preexistente en la TUI,
    se estrenó en la GUI.
  - `render_viewer` NO usa `uniform_list` (el `Viewer::rows(h)` ventana desde
    `v.scroll`, modelo cursor, no índice absoluto → desajuste con uniform_list):
    pinta `v.rows(H)` PLANO (H = alto del viewport / ROW_H), bounded a H filas
    (O(H), no O(total); #87 no aplica), `v.scroll` único dueño + rueda cableada.
  - `OpenViewer` con guard de generación (`viewer_gen`) + estado «abriendo
    visor…»: un `ViewerOpened` tardío (read lento) no abre el visor por sorpresa;
    cerrar invalida el pending.
  - `plugin.preview` primero → `fs.read` acotado (`FS_READ_MAX_CHUNK`);
    `truncated` = tope alcanzado (heurística conservadora). Status del visor en
    ES hardcodeado hasta GUI-e (el `Viewer` core se extrajo SIN i18n; TUI compone
    con Fluent). Dos resolvers (Browse+Viewer) validados contra la unión de
    comandos. `ByteRange` = `{offset, len: Option<u64>}`.
- Estado previo: aprobado (oscar); plan `docs/superpowers/plans/2026-07-20-gui-d-viewer.md`
- Contexto: M5 hito 2 = MVP de la GUI. Sub-proyectos: GUI-a (nav), GUI-b
  (mutaciones), GUI-c (keymap) — IMPLEMENTADOS en main; **GUI-d** (este, viewer),
  GUI-e (i18n+AccessKit). Construye sobre `crates/norte-gui` (EXCLUIDO) +
  `norte-frontend` (crate compartido puro) + el motor de keymap de GUI-c.

## Objetivo

`pane.view` (F3) sobre un archivo abre un VISOR a pantalla completa: texto con
encoding detectado, hexview para binarios, «recargar como…» (ciclo de encoding),
scroll. F3 intenta PRIMERO un previewer de plugin (`plugin.preview`) y cae a la
vista cruda si ninguno aplica. Las teclas del viewer son CONFIGURABLES (contexto
`Viewer` del keymap de GUI-c). Extrae el `Viewer` puro de la TUI a
`norte-frontend`.

## Decisiones (con el porqué)

1. **Extraer el CORE del `Viewer` a `norte-frontend::viewer`, SIN i18n** (Q1).
   El `Viewer` de `norte-tui::viewer` es PURO salvo dos acoplamientos:
   `norte_i18n::t` (solo en `status()`) y `crate::app::display_name` (en
   `with_plugin_preview`). El segundo se resuelve solo: `display_name` YA vive en
   `norte-frontend` (extraído en GUI-a). El primero se deja RENDER-side: el
   `status()` (composición i18n del texto de estado) NO se mueve; el `Viewer`
   puro expone getters (`encoding_name()`, `eol()`, `had_errors()`,
   `is_forced()`, + los ya públicos `hex`, `truncated`, `preview_plugin()`) y
   cada frontend compone su status (TUI localiza con Fluent; la GUI hardcodea ES
   hasta GUI-e). Motivo: `t!` sin bundle cargado en la GUI daría claves crudas
   (`"viewer-binary"`); y mantiene `norte-frontend` sin dep de i18n (como el
   keymap dejó la ayuda en la TUI). La TUI re-exporta/consume; `just ci` verde.

2. **Previewer de plugins primero, cruda como fallback** (Q2). F3 → la sesión
   llama `plugin.preview(path)`: `Some(PluginPreview{plugin_name, output})` →
   `Viewer::with_plugin_preview(path, plugin_name, &output)`; `None` →
   `fs.read(path, bounded)` → `Viewer::new(path, bytes, truncated)`. El core
   ejecuta el previewer APROBADO+ACTIVADO (gobierno M4); la GUI no conoce
   plugins. Async por la sesión persistente (muestra «cargando…»). Regla 9: todo
   por el daemon.

3. **Teclas del viewer configurables** (Q3). El motor de keymap de GUI-c ya
   soporta `Screen::Viewer`. La GUI construye DOS `Effective`/`Resolver` (Browse
   + Viewer, `build_for(Screen::Browse|Viewer)`); el resolver ACTIVO depende de
   la pantalla. `COMMANDS` de viewer: `viewer.close`, `viewer.up`, `viewer.down`,
   `viewer.page-up`, `viewer.page-down`, `viewer.top`, `viewer.bottom`,
   `viewer.encoding` (recargar como…), `viewer.encoding-auto` (reset),
   `viewer.hex` (toggle). `pane.view` se añade a las `COMMANDS` de Browse (F3).
   El `orthodox.toml` de la GUI gana una sección `[viewer]`.

## Datos verificados (del código, 2026-07-20)

- `norte_tui::viewer::Viewer` (PURO salvo lo dicho): campos pub `path: VPath`,
  `truncated: bool`, `hex: bool`, `scroll: usize`; privados `bytes`, `forced:
  Option<&'static Encoding>`, `plugin_preview`, `text`, `encoding_name:
  &'static str`, `eol: Eol`, `had_errors`, `lines`. Métodos: `new(path, bytes,
  truncated)`, `with_plugin_preview(path, plugin_name, output)` (usa
  `crate::app::display_name` para enmascarar), `preview_plugin() ->
  Option<&str>`, `cycle_encoding()`, `reset_encoding()`, `toggle_hex()`,
  `total_rows()`, `scroll_down/up(n)`, `scroll_top()`, `scroll_bottom()`,
  `rows(height) -> Vec<String>` (tabs expandidos + controles → `�`, display-safe
  spec §6), `status() -> String` (i18n — SE QUEDA render-side). Free fns
  `render_line` (privado), `hex_rows` (privado). Consts `PAGE=10`,
  `HEX_COLS=16`, `TAB_WIDTH=8`. `PluginPreviewView{plugin_name, lines}`.
  Único uso de `norte_i18n::t` = `status()`. La ref a "ratatui" es un COMENTARIO.
- `norte_frontend::display_name(bytes) -> (String, bool)` (ya extraído, GUI-a).
- `norte_encoding` (dep permisiva YA en `norte-frontend`): `detect`, `decode`,
  `decode_forced`, `detect_eol`, `reload_cycle`, `Decoded`, `Detection`, `Eol`,
  `Encoding`.
- `norte_proto::methods`: `PLUGIN_PREVIEW`; `PluginPreviewParams{path}`;
  `PluginPreviewResult{ #[serde(flatten)] preview: Option<PluginPreview> }`;
  `PluginPreview{plugin_id, plugin_name, output: String}` (all-or-nothing).
  `FS_READ = "fs.read"`, `FS_READ_MAX_CHUNK = 8 MiB`.
- `norte_core::backend`: `Backend::read(&VPath, Option<ByteRange>) ->
  Result<Vec<u8>, Error>`; hay un método `preview(&VPath) ->
  Result<PluginPreviewResult, Error>` en `RemoteBackend` (dentro de `#[cfg(unix)]`
  — el plan añade/usa un `Backend::preview` wrapper si no está en el enum).
  `ByteRange{off, len}` (transfer.rs).
- `norte_proto::Entry.size: Option<u64>` (para marcar `truncated` si `size >
  budget`).
- GUI (post GUI-c): `struct NorteGui` con `resolver: Resolver` (Browse),
  `keymap::{COMMANDS, build_effective, gpui_chord, run_command}`; `on_key`
  (modal → quick → resolver); render con `uniform_list` (#87); `session.rs`
  (`SessionCmd{List, Submit, Cancel}` / `SessionEvent{...}`, backend persistente).

## Arquitectura

### Componente A — `norte-frontend::viewer` (Viewer core, sin i18n)
- Mueve `Viewer` + `PluginPreviewView` + `render_line` + `hex_rows` + consts.
  `with_plugin_preview` usa `crate::display_name` (norte-frontend). BORRA
  `status()` del `Viewer` (i18n) y añade getters: `encoding_name() -> &str`,
  `eol() -> Eol`, `had_errors() -> bool`, `is_forced() -> bool`. Deps: ninguna
  nueva (norte-encoding + norte-proto ya están). Los tests del `Viewer` se
  mueven con el código.

### Componente B — TUI refactorizada
- `norte-tui::viewer` re-exporta `norte_frontend::viewer::{Viewer,
  PluginPreviewView, PAGE}` y aporta una fn libre `status(v: &Viewer) -> String`
  (la composición i18n movida, sobre los getters). Los call-sites (`ui.rs`
  `viewer.status()` → `viewer::status(viewer)`; `main.rs` construye el viewer)
  siguen resolviendo. `just ci` verde.

### Componente C — GUI: pantalla viewer + previewer + keymap
- Estado: `viewer: Option<norte_frontend::viewer::Viewer>` en `NorteGui`
  (`None` = dual-pane; `Some` = viewer a pantalla completa). Dos resolvers:
  `resolver` (Browse, ya existe) + `viewer_resolver` (nuevo, Screen::Viewer);
  `keymap::build_effective` construye ambos (o una variante `build_effectives()`
  que devuelve los dos). `COMMANDS` de viewer + `pane.view` en Browse.
- `run_command` (Browse) gana `"pane.view" => self.open_viewer(cx)`:
  `open_viewer` toma el `selected()` del pane activo si es `File` y manda
  `SessionCmd::OpenViewer{path}`; muestra un estado de carga.
- `run_viewer_command(&mut self, cmd, cx)` (nuevo, contexto Viewer):
  `viewer.close`→`self.viewer = None`; `viewer.up/down`→`scroll_up/down(1)`;
  `viewer.page-up/down`→`scroll_up/down(PAGE)`; `viewer.top/bottom`→
  `scroll_top/bottom`; `viewer.encoding`→`cycle_encoding`; `viewer.encoding-auto`
  →`reset_encoding`; `viewer.hex`→`toggle_hex`. (Todos sobre `self.viewer` si
  `Some`.)
- `on_key`: si `self.viewer.is_some()` → enruta al `viewer_resolver` +
  `run_viewer_command` (el modal/quick no aplican en el viewer). Si no → el flujo
  de Browse de GUI-c.
- **Session:** `SessionCmd::OpenViewer{path}` → el hilo tokio: (1) `backend.preview(path)`
  → si `Some(p)` emite `SessionEvent::ViewerOpened{path, ViewerContent::Plugin{plugin_name, output}}`;
  (2) si `None`, lee bytes acotados: `backend.read(path, Some(ByteRange{off:0,
  len:FS_READ_MAX_CHUNK}))` → `SessionEvent::ViewerOpened{path,
  ViewerContent::Raw{bytes, truncated}}` (truncated = `size > budget`, con `size`
  del Entry o re-stat). Error → `SessionEvent::ViewerFailed{path, error}` → banner.
- `apply_event`: `ViewerOpened{Plugin}` → `Viewer::with_plugin_preview(...)`;
  `ViewerOpened{Raw}` → `Viewer::new(...)`; en ambos `self.viewer = Some(v)`.
- **Render:** si `self.viewer` es `Some`, pinta el visor a pantalla completa
  (reemplaza el dual-pane): cabecera (path saneado con `path_display` + indicador
  «via <plugin>» si preview), las `viewer.rows(height)` VIRTUALIZADAS con
  `uniform_list` (patrón #87; alto de fila `ROW_H`), y una barra de status
  compuesta EN LA GUI (ES): encoding/binario, EOL, lossy, truncated (a partir de
  los getters). Los `rows()` ya vienen display-safe.

## Manejo de errores
- `preview`/`read` fallan → `ViewerFailed` → banner, el viewer NO se abre, sin panic.
- Archivo enorme → lectura acotada a `FS_READ_MAX_CHUNK`, `truncated=true` en el
  status (el usuario SIEMPRE sabe que ve una cabecera).
- Binario → hexview automático (nunca decodificar a ciegas, spec §6).
- F3 sobre un dir/symlink → no-op (solo `File`).

## Testing
- **norte-frontend (gate `just ci`):** los tests del `Viewer` MOVIDOS (decode/
  detección, hex_rows, ciclo de encoding, scroll+clamp, binario→hex, EOL,
  `with_plugin_preview` enmascara la salida del plugin) verdes una vez; +
  getters. `render_line` (tabs+controles) y `rows()` display-safe cubiertos.
- **norte-tui (gate):** sin regresión (consume por re-export; la fn `status`
  localiza sobre los getters).
- **norte-gui (excluido, spike):** `run_viewer_command` puro (efecto sobre el
  `Viewer`), la composición del status GUI con test hostil (encoding/EOL/nombre
  no dejan hazards crudos), `gpui_chord` ya cubierto (GUI-c). Verificación
  manual: F3 sobre texto (encoding), binario (hexview), «recargar como…» (ciclo),
  hex toggle, close; previewer si hay un plugin aprobado.

## Fuera de alcance (posterior)
- Preview de imágenes/media (el previewer devuelve TEXTO; sin rasterizado).
- Edición en el viewer, búsqueda dentro del viewer.
- i18n de la GUI (el status del viewer se hardcodea en ES; GUI-e).
- Hot-reload del keymap; unificar el `Pane` TUI sobre PaneState (#82).

## Riesgo
La extracción del `Viewer` (refactor TUI + mover la composición i18n del status
al render) y el cambio de contexto Browse↔Viewer en el keymap (dos resolvers)
son los puntos delicados — mitigados con re-export + `just ci` verde en cada
paso + los tests del `Viewer` movidos (patrón GUI-a/GUI-c). El `plugin.preview`
reusa el método ya existente del backend (M4), sin cambio de wire.
