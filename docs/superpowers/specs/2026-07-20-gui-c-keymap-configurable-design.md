# GUI-c — keymap configurable (M5 hito 2, sub-proyecto 3) — diseño

- Fecha: 2026-07-20
- Estado: **IMPLEMENTADO** (T1–T4, cerrado 2026-07-20; commits 6ca356b..341b12f,
  rama `gui-c-keymap`). `just ci` EXIT=0; norte-frontend 47 tests + norte-tui
  207 + norte-gui 24 + clippy limpio. Reviewers rust (por task) + encoding
  (adaptadores de chord: `char` correcto, sin truncado) aplicados.
  **Verificación GUI interactiva PENDIENTE de oscar** (headless), pero los checks
  del plan Step 6 están AUTOMATIZADOS (tests e2e vía `NORTE_CONFIG_DIR`: rebind
  j/k, comando desconocido→error, `lua:` inerte).
  Desviaciones (deuda anotada):
  - El motor exigió un tipo de tecla NEUTRO (`KeyCode`/`Mods`/`Chord` sin
    crossterm); adaptadores `chord_from_crossterm` (TUI) y `gpui_chord` (GUI).
    Fix de review T2: `Resolver::reset()` — una tecla no modelada (`None` del
    adaptador) rompe la secuencia multi-tecla (paridad con el `from_event` viejo).
  - Preset GUI orthodox PROPIO (solo comandos de la GUI): salir = **F10**/ctrl+c
    (NO `q` — una letra suelta abriría quick-search, el modelo type-to-filter de
    la GUI). Modales con teclas FIJAS; quick-search fallthrough.
  - `input.rs`/`key_to_action` BORRADOS (huérfanos tras el rewire del resolver).
  - Guard `platform` (Super/Cmd) en `on_key`: no rutea al resolver (evita que
    Cmd+q colapse a `q`).
  - `Resolution::Pending` sin indicador visual en la GUI (no hay secuencias
    multi-tecla en el preset; deuda si se añaden). Hot-reload, contexto viewer,
    host Lua, cheatsheet, i18n = fuera de alcance (GUI-d/e). Charset `valid_name`
    duplicado motor↔lua = **issue #88**.
- Estado previo: aprobado (oscar); plan `docs/superpowers/plans/2026-07-20-gui-c-keymap-configurable.md`
- Contexto: M5 hito 2 = MVP de la GUI. Sub-proyectos: GUI-a (nav read-only, IMPLEMENTADO),
  GUI-b (mutaciones, IMPLEMENTADO+merge), **GUI-c** (este, keymap configurable),
  GUI-d (viewer), GUI-e (i18n+AccessKit). Construye sobre `crates/norte-gui` (EXCLUIDO
  del workspace) + el crate compartido `norte-frontend`.

## Objetivo

La GUI resuelve teclas→comandos vía un motor de keymap **configurable** (mismo
formato/motor que la TUI), reemplazando el `input::key_to_action` hardcodeado.
Preset por defecto + capas sistema/usuario/proyecto. Sin Lua, sin hot-reload
(diferidos). El usuario puede rebindear cualquier acción de navegación/mutación
del contexto Browse editando un `keymap.toml`, con el MISMO formato que la TUI.

## El crux: desacoplar el motor de crossterm (tipo de tecla NEUTRO)

Hoy el motor de keymap vive ATRAPADO en `norte-tui::keymap` y su `Chord` usa
`crossterm::event::{KeyCode, KeyModifiers}` (lib de TERMINAL). `norte-frontend`
es un crate PURO sin deps de UI (ni ratatui/crossterm ni gpui), y la GUI (GPUI)
no usa crossterm. Por tanto la extracción NO es "mover el fichero": exige un
**tipo de tecla neutro** en `norte-frontend`, y que cada frontend convierta su
evento nativo a ese tipo.

- `norte-frontend::keymap` define `KeyCode` (neutro) + `Mods` + `Chord`. El
  `KeyCode` neutro espeja EXACTAMENTE el set que ya acepta `parse_chord`:
  `Char(char)`, `F(u8)` (1..=12), `Enter`, `Tab`, `Esc`, `Backspace`, `Up`,
  `Down`, `Left`, `Right`, `Home`, `End`, `PageUp`, `PageDown`, `Insert`,
  `Delete`. `Mods` = `{ctrl, alt, shift}` (super/platform NO lo usa el formato
  actual; se puede añadir sin romper). Espacio = `Char(' ')` (como hoy).
- El PARSER (`parse_chord`/`parse_keymap`) es puro texto→neutro (`"ctrl+f5"`,
  `"g g"`), independiente del frontend: se mueve tal cual, solo cambia los tipos
  crossterm por los neutros.
- **Adaptadores por frontend** (fuera del motor):
  - TUI: `crossterm KeyCode/KeyModifiers → Chord` (el actual `Chord::from_event`
    se convierte en un adaptador TUI-side: `impl From<(KeyModifiers, KeyCode)>`
    o una fn en `norte-tui`; conserva la regla "en `Char` el shift ya está en el
    char, se descarta").
  - GUI: `nombre-de-tecla-GPUI + modifiers → Chord`. La GUI ya parsea nombres
    (`"up"`, `"f5"`, `"tab"`, `"insert"`… en `input::key_to_action`); ese mapeo
    se reusa para construir un `Chord` neutro (con `key_char` para fidelidad de
    layout en `Char`).

## Decisiones (con el porqué)

1. **Motor compartido en `norte-frontend::keymap`** (extracción incremental,
   patrón GUI-a). Se mueven: `KeyCode/Mods/Chord` (neutros), `parse_chord`,
   `KeymapFile`/`RawSection`/`RawBinding`/`parse_keymap`, `Screen`, `Effective`
   (merge + capas), `Resolver`, `Resolution`/`Lookup`, `KeymapError`, el formato
   de preset. El motor YA está parametrizado por la lista de comandos
   (`Effective::build(preset, user, known_commands)` / `build_layered` /
   `build_for`), así que NO conoce comandos concretos — cada frontend le pasa su
   `COMMANDS`. La TUI se refactoriza para consumir el crate (re-export; `just
   ci` verde en CADA paso). `norte-frontend` gana dep `serde`/`toml` para el
   parseo (permisivas; ya presentes en el workspace).
2. **Capas completas sistema/usuario/proyecto** (paridad con la TUI): el motor
   ya las soporta (`build_layered`/`build_for`, prepend/append estilo Yazi). El
   descarte de bindings `lua:` de capa-PROYECTO (`KeymapFile::mark_project` +
   `discarded_lua_bindings`) se CONSERVA en el motor (inerte sin Lua, pero
   mantiene la garantía de seguridad y la paridad; el caller lo pinta una vez).
3. **Sin Lua en la GUI**: el `run` de un binding es un nombre de comando de la
   lista `COMMANDS` de la GUI. Un `run = "lua:..."` que el usuario ponga se
   IGNORA con aviso (la GUI no monta el host Lua — subsistema `norte-tui/lua/`,
   regla 9, fuera de alcance). El motor sigue tratando `run` como string opaca.
4. **Carga al arranque; hot-reload diferido**: la GUI construye el `Effective`
   una vez al arrancar. Reconstruir un `Effective` nuevo ya es posible (ADR
   0007), así que añadir un watcher de fichero luego es incremental.
5. **Presets POR frontend**: el FORMATO+parser se comparten, pero el CONTENIDO
   del preset es propio de cada frontend. Un preset compartido con comandos
   `viewer.*`/`app.extensions`/`lua` NO validaría contra el set de la GUI
   (`Effective::build` falla ante un comando desconocido — garantía deseada).
   La GUI trae su `orthodox.toml` con SOLO sus comandos. Default GUI = orthodox.
   (Si en el futuro se quiere un preset "común" compartido, será un refactor
   aparte; GUI-c no lo necesita.)
6. **Contextos**: GUI-c usa SOLO el contexto Browse (el viewer es GUI-d). Los
   **MODALES siguen con teclas FIJAS** (`y`/`n`/`p`/`o`/`s`/`c`): son prompts de
   overlay, no configurables (mismo criterio que los overlays de la TUI, #24).
   El quick-search (teclear un imprimible ABRE/alimenta el filtro) es un
   FALLTHROUGH, NO un binding — se preserva tal cual.
7. **`COMMANDS` de la GUI** (contexto Browse): `app.quit`, `pane.switch`,
   `cursor.up`, `cursor.down`, `cursor.top`, `cursor.bottom`, `cursor.page-up`,
   `cursor.page-down`, `nav.enter`, `nav.parent`, `mark.toggle`, `pane.copy`,
   `pane.move`, `pane.delete`, `task.cancel`. Nombres alineados con los de la
   TUI donde coinciden (`COMMANDS` de `norte-tui`). `pane.delete-permanent` NO
   es comando propio: el borrado permanente se elige en el modal (tecla `p`),
   no por binding.

## Arquitectura

### Componente A — `norte-frontend::keymap` (motor extraído + tecla neutra)
- Módulo nuevo `keymap` (o submódulos) con los tipos/lógica movidos de
  `norte-tui::keymap`, sobre el `KeyCode/Mods/Chord` neutros. Deps nuevas del
  crate: `serde` + `toml` (parseo del keymap; permisivas). `#![forbid(unsafe)]`
  ya activo.
- API pública consumida por ambos frontends: `Chord`, `KeyCode`, `Mods`,
  `parse_chord`, `KeymapFile`, `parse_keymap`, `Screen`, `Effective::{build,
  build_layered, build_for}`, `Resolver::{new, push, pending}`, `Resolution`,
  `KeymapError`, y un `presets`-format helper (NO los presets concretos de la
  TUI). `Effective::bindings()`/`discarded_lua_bindings()` se conservan.
- Los TESTS del motor que hoy viven en `norte-tui::keymap` se MUEVEN con el
  código (prefix-free, merge de capas, parse, resolución de secuencias, Esc
  cancela) y siguen verdes UNA vez.

### Componente B — TUI refactorizada (consume el motor)
- `norte-tui::keymap` pasa a: (a) re-exportar del crate compartido lo movido;
  (b) conservar su `COMMANDS` (lista TUI), su `presets()` (vim/orthodox con
  comandos TUI), el `help_id`, y (c) el adaptador `crossterm → Chord`
  (`from_event`). Los call-sites de `main.rs` (`Resolver::new`, `push`,
  `Effective::build_for`) siguen resolviendo vía re-export. `just ci` verde.

### Componente C — GUI: keymap propio sobre el motor
- `norte-gui`: `COMMANDS` (browse, §7), un `orthodox.toml` embebido
  (`include_str!`) como preset por defecto, y un `command → acción` map (una fn
  pura `run_command(&mut self, cmd: &str, cx)` que hace `match cmd`) que
  REEMPLAZA el `key_to_action` hardcodeado para las acciones nombradas.
- Adaptador `gpui_chord(key: &str, mods, key_char) -> Option<Chord>`: convierte
  el `KeyDownEvent` de GPUI a un `Chord` neutro (reusa el vocabulario de nombres
  de `input.rs`; `Char` usa `key_char` para fidelidad de layout).
- Config loader: resuelve las rutas de capa (config dir XDG — `NORTE_CONFIG_DIR`
  → XDG → `~/.config/norte`, igual que la TUI/norte-connect) y parsea los
  `keymap.toml` de sistema/usuario/proyecto en `KeymapFile`s; marca la de
  proyecto (`mark_project`). Construye `Effective::build_for(orthodox, layers,
  COMMANDS, Screen::Browse)` → `Resolver`. Errores de carga → banner, jamás
  panic; keymap inválido → cae al preset con aviso (fail-safe, no fail-closed:
  un typo no debe dejar la GUI sin teclas).
- Flujo en `on_key`: (1) modal abierto → teclas FIJAS del modal (sin resolver);
  (2) si no, quick-search activo → imprimibles al filtro (fallthrough actual);
  (3) si no → `gpui_chord(...)` → `resolver.push(chord)`:
  `Resolution::Run(cmd)` → `run_command(cmd)`; `Resolution::Pending(_)` → (sin
  UI de secuencia en GUI-c, o un indicador mínimo al pie); `Resolution::Reset` →
  si la tecla es un imprimible, ABRE quick-search (fallthrough). El ratón
  (click/rueda) NO pasa por el keymap (acciones GUI directas, como hoy).

## Manejo de errores
- keymap.toml ausente → solo el preset (comportamiento por defecto).
- keymap.toml inválido (parse/chord/comando desconocido/secuencia no prefix-free)
  → NO se aplica esa capa; banner con el diagnóstico de `KeymapError`; la GUI
  sigue con el preset. Nunca panic, nunca GUI sin teclas.
- binding `lua:` → ignorado + aviso una vez (sin host Lua).

## Testing
- **norte-frontend (gate `just ci`):** los tests del motor MOVIDOS de la TUI
  (parse, prefix-free, merge de capas, resolución de secuencias, Esc, descarte
  lua de proyecto) siguen verdes una vez; + un test de que `build` valida contra
  un `known_commands` PARAMETRIZADO (un comando fuera de la lista → `KeymapError`).
- **norte-tui (gate):** sin regresión (consume el motor por re-export); sus
  tests de keymap siguen verdes.
- **norte-gui (excluido, política spike — sin tests de render):** fn puras CON
  test: `gpui_chord` (nombre GPUI → `Chord` neutro, incl. `Char` con key_char,
  `f5`, `ctrl+`, teclas sin binding → `None`), y `run_command` mapea cada comando
  a su efecto sobre el estado (testeable sin GPUI donde el efecto sea puro; los
  que abren modal/mandan `SessionCmd` se verifican por el `Modal`/comando
  resultante). Verificación manual: rebindear una tecla en `keymap.toml` (p. ej.
  `nav.enter` a otra tecla) y ver el efecto contra el daemon.

## Fuera de alcance (sub-proyectos/tasks posteriores)
- Host Lua en la GUI (bindings `lua:`).
- Hot-reload del keymap (watcher).
- Contexto viewer (GUI-d).
- Overlay de ayuda/cheatsheet de bindings y command palette.
- i18n de los mensajes (GUI-e).
- Preset "común" compartido entre frontends (hoy cada uno trae el suyo).
- Config de tema/otras secciones de config (solo keymap).

## Riesgo
El refactor de la TUI (extraer su motor de keymap y adaptarla a la tecla neutra)
es el punto delicado — el `Chord` deja de ser crossterm y la TUI gana un
adaptador. Mitigado con re-export + `just ci` verde en CADA paso de la
extracción, y moviendo los tests del motor con el código para detectar cualquier
cambio de comportamiento al instante (patrón GUI-a).
