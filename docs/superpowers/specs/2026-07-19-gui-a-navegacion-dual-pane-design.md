# GUI-a — navegación dual-pane (M5 hito 2, sub-proyecto 1) — diseño

- Fecha: 2026-07-19
- Estado: aprobado (oscar); pendiente de plan
- Contexto: M5 hito 2 = MVP de la GUI (spec §283, criterio de salida M5:
  «GUI y TUI sobre la misma sesión simultáneamente» — ya demostrado read-only
  en el spike, ADR 0027 GO). El MVP es un MILESTONE, descompuesto en
  sub-proyectos: **GUI-a** (este, navegación dual-pane read-only), GUI-b
  (mutaciones), GUI-c (keymap configurable), GUI-d (viewer), GUI-e (i18n +
  AccessKit). Cada uno spec→plan→impl propio. Construye sobre el scaffold
  GPUI de `crates/norte-gui` (spike: ventana + conexión daemon + listado +
  theme_map).

## Objetivo

Convertir el pane de listado único del spike en un dual-pane NAVEGABLE
read-only: dos panes con foco conmutable, cursor, cd/enter/parent, quick
search por tipeo, teclado y ratón básico. Cero mutación.

## Decisiones (con el porqué)

1. **Crate `norte-frontend` (lib) para la lógica de presentación PURA.**
   Hoy `display_name`/`path_display`/`sort_entries` (norte-tui::app) y
   `nav::QuickSearch` (norte-tui::nav) viven ATRAPADOS en el crate binario
   norte-tui; la GUI los necesita igual. Se extraen a un crate lib SIN deps
   de UI (ni ratatui ni gpui), que TUI y GUI consumen — cero duplicación,
   testeable una vez, patrón Zed (crates compartidos entre frontends).
   Licencia Apache/MIT (presentación, sin negocio; como norte-theme).
2. **Extracción INCREMENTAL, guiada por necesidad (YAGNI).** GUI-a extrae
   SOLO lo que consume: `display_name`, `path_display`, `sort_entries`,
   `nav::QuickSearch`, y un `PaneState` nuevo (la parte NO-render del `Pane`
   de la TUI). El motor de keymap (`Effective`/`Resolver`) y la config de
   capas se quedan en la TUI por ahora — se extraen cuando lleguen GUI-c y
   la config. No se refactoriza de más.
3. **La TUI se refactoriza para consumir norte-frontend** (no duplica): sus
   copias locales de lo extraído se sustituyen por el uso del crate nuevo.
   `just ci` verde en CADA paso (los tests que ya cubren `sort_entries`/
   `QuickSearch`/`display_name` en la TUI se MUEVEN con el código y siguen
   verdes).
4. **Teclas hardcodeadas mínimas** en GUI-a (el keymap configurable es
   GUI-c). Ratón básico donde es natural en GUI (click=foco+cursor,
   doble-click=entra, rueda=scroll) — aprovecha que es GUI sin reimplementar
   el modelo ortodoxo.
5. **Read-only, siempre por daemon.** Solo `fs.list`. Sin mutación (GUI-b),
   sin modo embebido.

## Componentes

### `crates/norte-frontend` (lib nueva)

- `display_name(bytes: &[u8]) -> (String, bool)` y
  `path_display(p: &VPath) -> (String, bool)` — saneado de nombres/paths
  hostiles (mask de controles/bidi/invisibles vía
  `norte_encoding::mask_terminal_hazards` + lossy; el bool = «hostil»,
  para el badge). Fuente ÚNICA; la TUI pasa a llamarlos aquí.
- `sort_entries(entries: &mut [Entry])` — orden de listado (dirs primero,
  luego por nombre NFC; el criterio EXACTO se preserva del actual de la TUI).
- `nav::QuickSearch` (+ `Mode{Filter,Jump}`, `matches`) — filtro/salto puro,
  con sus tests (se mueven tal cual de norte-tui::nav; `History`/`Hotlist`
  se quedan en la TUI por ahora — GUI-a no los usa, YAGNI).
- `PaneState` (nuevo): estado PURO de un pane, sin render.
  - Campos: `dir: VPath`, `entries: Vec<Entry>`, `cursor: usize`,
    `loading: bool`, `quick: Option<QuickSearch>`.
  - Métodos: `new(dir, entries)`, `set_listing(dir, entries)` (reset cursor,
    cierra quick), `cursor_up/down`, `page_up/down(n)`, `home/end`,
    `selected() -> Option<&Entry>` (respeta el filtro quick, como el Pane de
    la TUI), `quick_start/char/backspace/cancel/confirm`,
    `quick_visible() -> Option<&[usize]>`.
  - Es la extracción de la parte no-render del `Pane` de norte-tui::app; la
    TUI puede seguir con su `Pane` (render) envolviendo o consumiendo este
    `PaneState` — decisión de la refactorización, sin romper la TUI.

### `crates/norte-gui` (construye encima del scaffold)

- `AppState`: `[PaneState; 2]` + `focus: usize` (0|1) + `Theme` cacheado
  (`preset_default`).
- `backend`: el `backend_task.rs` del spike, generalizado — un
  `RemoteBackend` compartido; `list(dir) -> Result<Vec<Entry>, Error>` async
  que cruza al hilo GPUI por `oneshot` (patrón del spike). Un `cd` = un
  `list` del pane con foco.
- `render` (GPUI): contenedor horizontal, dos columnas (los panes); cada
  columna una lista vertical con `display_name` + color por tipo
  (`theme_map::to_gpui_rgba`, ya existe) + indicador (dir/file/symlink);
  pane con foco resaltado; cursor resaltado; línea de quick al pie del pane
  activo cuando filtra.
- `input`: mapeo tecla/ratón → acción, hardcodeado.

## Interacción (hardcoded; configurable = GUI-c)

- **Foco**: `Tab` conmuta pane; click en un pane → foco + cursor a la
  entrada clicada.
- **Cursor**: `↑↓`; `Home/End`; `PgUp/PgDn`; rueda del ratón.
- **Navegar**: `Enter` / doble-click sobre dir → cd (list nuevo); sobre file
  → no-op en GUI-a (viewer = GUI-d). `Backspace` → padre.
- **Quick search por tipeo**: un carácter imprimible abre/alimenta el filtro
  incremental (`QuickSearch` modo Filter); el listado se reduce; `Esc`
  limpia/cierra; `↑↓` navegan lo filtrado; `Enter` confirma (entra si dir).
  Cada pane su quick.

## Datos y errores (read-only)

- `cd` async: pane → «cargando…», `list` en background, re-render con las
  entradas al llegar. Cero mutación.
- Error de `cd` (NotFound, provider caído) → la categoría del error en el
  pane (texto), JAMÁS panic. Daemon caído → mensaje en la ventana (scaffold).

## Tests

- `norte-frontend`: lo extraído llega con sus tests (los de `sort_entries`/
  `QuickSearch`/`display_name` que ya existen en la TUI se mueven con el
  código y siguen verdes en el gate). `PaneState` nuevo: tests de
  cursor/quick puros (sin GPU) — cursor con clamps, page, selected respeta
  el filtro, quick filter/jump.
- `norte-tui`: `just ci` verde tras la refactorización (usa norte-frontend,
  no duplica) — regresión cero.
- `norte-gui`: sin suite formal de render (política del spike); el mapeo
  input→acción, si sale como fn pura, lleva test.

## Fuera de alcance

Mutaciones copy/move/delete + modales + panel de tasks (GUI-b); keymap
configurable + config de capas (GUI-c); viewer F3 (GUI-d); i18n Fluent en la
GUI + AccessKit (GUI-e); historial/hotlist en la GUI (después); modo
embebido (siempre daemon); scroll virtual para dirs enormes (si el listado
completo en RAM molesta, optimización posterior — el daemon ya pagina, GUI-a
lista el dir de una).

## Criterio de salida

Con `norte daemon run`: la GUI abre dos panes navegables, `Tab` conmuta
foco, `↑↓`/Enter entran en dirs, `Backspace` sube, teclear filtra
incrementalmente, el ratón enfoca/entra — todo read-only contra el daemon,
con los colores de norte-theme; la TUI sigue verde (`just ci`) reusando
`norte-frontend`; el `PaneState`/`QuickSearch`/saneado están testeados una
sola vez en el crate compartido.
