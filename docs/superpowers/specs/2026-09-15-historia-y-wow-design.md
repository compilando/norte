# Historia de navegación completa, splash y las ocho mejoras «WOW»

**Fecha:** 2026-09-15. **Estado:** aprobado por Oscar en conversación
(«impleméntalo de la forma más extensible posible, para TUI y GUI, si es
posible o conveniente como plugin; y añade la historia de Krusader, analizada
entera»). Las decisiones abiertas se toman aquí y se dicen; no se paró a
preguntar.

Es un PROGRAMA, no una rama: diez fases, cada una su rama, su plan y su gate.
Este documento fija el orden, las piezas compartidas y las decisiones de cada
fase. La fase 1 va detallada; las demás llevan lo que su plan no puede
decidir solo.

## Reglas transversales

1. **Las dos superficies, una decisión.** La regla va a `norte-frontend` (o
   `norte-ui-host` si es estado de ventana); TUI y ventana pintan. Cada fase
   cierra con un test de paridad (ADR 0077).
2. **Plugin cuando cabe.** Se pregunta en este orden: ¿cabe en un kind que ya
   existe? → plugin. ¿Hace falta un kind nuevo que sirva a más de un caso? →
   kind nuevo con ADR. ¿Necesita tareas cancelables, el journal o la policy?
   → core, y la ADR dice por qué no es plugin.
3. **Pintura por datos, no por `match`.** La fase 3 introduce `StyledFrame`:
   líneas de spans con rol/fg/bg + zonas clicables que nombran comandos. Lo
   pintan UNA función de la TUI y UNA del renderer. Los paneles nuevos de este
   programa (mapa de disco, línea de tiempo, git) producen un `StyledFrame`,
   sean plugin o built-in; ningún panel nuevo añade un brazo a `panels.rs` ni
   un `if (slot.kind === …)` a `render.ts`.
4. **Configurable cuando hay algo que elegir**, con fila en ajustes, Fluent
   `en`/`es`, golden del schema y ayuda.
5. **Teclas en los siete presets** o motivo escrito en la cabecera
   (CLAUDE.md). Los cuatro importados son TRANSCRIPCIONES.

## Orden

| fase | rama | qué | wire | ADR |
| --- | --- | --- | --- | --- |
| 1 | `feat/historia-de-navegacion` | historia completa | no (sesión es cuerpo de frontend) | 0114 |
| 2 | `feat/splash-y-cromo` | splash, procesos que no roban pantalla, retoques visibles | no | 0115 |
| 3 | `feat/panel-kind` | kind `panel` + `StyledFrame` + plugin `git-panel` | sí | 0116 |
| 4 | `feat/mapa-de-disco` | `fs.dir_usage` + treemap | sí | 0117 |
| 5 | `feat/imagenes-en-terminal` | protocolo gráfico kitty en la TUI | no | 0118 |
| 6 | `feat/ir-a-cualquier-sitio` | caja única con fuentes | no | 0119 |
| 7 | `feat/linea-de-tiempo` | `journal.list` + deshacer hasta un punto | sí | 0120 |
| 8 | `feat/plan-de-organizar` | kind `organizer` + `ai.organize_plan` | sí | 0121 |
| 9 | `feat/handoff` | pasar la sesión entre TUI y ventana | sí | 0122 |

La fase 1 va primero porque Oscar la pidió explícitamente. La 3 va antes que
4/7 porque les da la pintura. Los números de ADR son provisionales: el
siguiente libre al escribirla.

---

## Fase 1 — La historia de navegación, entera

### Lo que hay (verificado 2026-09-15)

- `norte_frontend::nav::History` es UNA implementación compartida (ADR 0066
  D14): MRU de 30 con dedup consecutivo, rastro `back`/`fwd` estilo
  navegador (un `record` vacía `fwd`), por HUECO — cada pestaña la suya —, que
  viaja con el contenido en `pane.swap`, y cuyo rastro se guarda en la sesión
  (`SlotState{back,forward}`; el MRU se reconstruye del rastro).
- Comandos: `nav.back`/`nav.forward` (con contador), `pane.history` (lista),
  `pane.hotlist` (favoritos).
- La lista es una foto congelada: TUI `NavPopup`, ventana `Selector::historial`.
  Solo navega; no filtra, no borra, no marca dónde estás.

### Lo que dicen los gestores (fuentes)

| gestor | atrás/adelante | lista | extras |
| --- | --- | --- | --- |
| **Krusader** (código fuente, `listpanelactions.cpp`) | `Alt+←/→` (`KStandardAction::Back/Forward`) | `Ctrl+H`; `Ctrl+Alt+←/→` lista del panel IZQUIERDO/DERECHO | `Ctrl+J` saltar atrás al punto, `Ctrl+Shift+J` fijar punto; `Ctrl+Z` Popular URLs (ranking persistido, `kractions.cpp`); «Left/Right Bookmarks» SIN tecla |
| **Total Commander** (KEYBOARD.TXT 11.58, ya transcrito) | `Alt+←/→` | `Alt+↓` (filtrada: quita dirs de paso); `Alt+Shift+↓` sin filtrar | persistida por lado, 200 |
| **Far** (FarEng.hlf) | — (`Alt+←/→` desplaza nombres largos) | `Alt+F12` | en la lista: `Del` vaciar, `Shift+Del` quitar, `Ins` bloquear, `Ctrl+Shift+Enter` abrir en el pasivo, `Ctrl+Enter` ruta a la línea, `Ctrl+R` quitar muertas |
| **mc** (man) | `Alt+Y` / `Alt+U` | `Alt+Shift+H` | — |
| **Norton** | sin fuente primaria | — | — |
| **ranger / yazi** | `H` / `L` | — | — |
| **convención GUI** | `Alt+←/→`, botones X1/X2 del ratón | — | — |

**Hallazgo**: la cabecera de `krusader.toml` dice que `Alt+←/→` son «per-panel
bookmark menus» y deja `nav.back`/`nav.forward` sin atar. Viene de la tabla de
docs.kde.org, que está DESFASADA: en el código actual esas acciones no tienen
tecla y `Alt+←/→` son `Back`/`Forward` estándar de KDE. Se corrige.

### Decisiones

**D1. Teclas.** Nuevo en negrita.

| preset | `nav.back` / `nav.forward` | `pane.history` | nuevos |
| --- | --- | --- | --- |
| orthodox (mc) | alt+left/right, **alt+y** | alt+down, **alt+H** | — |
| vim | alt+left/right, **H / L** | alt+down | — |
| cua | alt+left/right | alt+down | — |
| total-commander | alt+left/right | alt+down | — |
| krusader | **alt+left / alt+right** | ctrl+h | **ctrl+alt+left/right** → `pane.history-left/-right`; **ctrl+j** `nav.jump-back`; **ctrl+z** `pane.popular` |
| far | — (motivo ya escrito) | alt+f12 | — |
| norton | — | — | cabecera: sin fuente primaria de historia |

- `alt+u` de mc NO se ata en orthodox: es `pane.pull` desde julio y un
  lector de norte lo tiene en los dedos. La cabecera lo dice; `alt+y` sin su
  pareja es honesto porque `alt+right` sigue siendo adelante.
- `Alt+Shift+↓` de TC no se ata: norte no filtra dirs «de paso», así que
  sería la misma lista con otra tecla. Cabecera.
- `ctrl+alt+←/→` solo en krusader: en GNOME/KDE esos acordes cambian de
  escritorio, así que en los nativos serían teclas que no llegan. Los
  nativos alcanzan la lista de cada lado por menú y paleta. Cabeceras.
- `Ctrl+Shift+J` de Krusader (fijar punto) NO se ata: es un
  `ctrl+<MAYÚSCULA>` y ningún terminal lo entrega distinto de `ctrl+j`
  (CLAUDE.md). `nav.set-jump-point` queda en menú y paleta; la cabecera lo
  dice.
- Jump point y popular: solo krusader los atesta; los demás los tienen en el
  menú Ir y en la paleta, sin tecla, y su cabecera lo dice.
- **Ratón**: en la ventana, los botones X1/X2 son `nav.back`/`nav.forward`
  siempre (convención de plataforma, como la rueda). La TUI no puede: crossterm
  no informa de esos botones — lo dice la ayuda.

**D2. Lista de historia: acciones (las dos superficies).** Las teclas de la
lista son de norte (`dialog_from = "orthodox"`), así que se eligen las de Far,
la única fuente que las itemiza:

| tecla (diálogo) | comando | efecto |
| --- | --- | --- |
| `enter` | `dialog.confirm` | ir (cuenta como navegación: entra en el rastro) |
| `ctrl+enter` | **`dialog.confirm-other`** | abrir en el OTRO panel, sin mover el foco |
| `delete`, `d` | `dialog.remove` | quitar esa entrada del MRU y del rastro |
| `shift+delete` | **`dialog.clear`** | vaciar la historia de ese panel (pide confirmación) |
| `a` | `dialog.add` | añadir a favoritos |
| `/` | `dialog.filter` | filtrar tecleando (subsecuencia, como la paleta) |

`dialog.confirm-other` y `dialog.clear` son comandos nuevos; los demás ya
existen con ese significado en la hotlist. `Ins`-bloquear de Far NO se copia:
una entrada bloqueada que sobrevive a todo es un favorito, y norte ya los
tiene — `a` hace eso. Copiar la ruta (`Ctrl+Enter` de Far) queda fuera: en la
TUI no hay portapapeles del sistema fiable, y en norte `ctrl+enter` es más útil
como «abrir en el otro».

**D3. La lista dice dónde estás.** Primera fila: el directorio ACTUAL,
marcado (como el check de Krusader) y no navegable. Debajo, el MRU. Las filas
que están en la rama `fwd` llevan una marca «adelante», porque el lector que
ha ido atrás tiene que ver que puede volver.

**D4. Tamaño configurable.** `[ui] history_size`, entero `5..=64`, por defecto
`30`. El techo es `64` porque es el tope que `SessionBody::prune` ya guarda por
hueco: uno más grande se perdería al reiniciar sin decir nada. `History` pasa
a llevar su cota (`History::with_capacity`); el invariante
`back.len() + fwd.len() <= cap` se conserva igual.

**D5. Punto de salto (Krusader Ctrl+J / Ctrl+Shift+J).** `History.jump:
Option<VPath>` por hueco. `nav.set-jump-point` lo fija en el dir actual (aviso
«punto de salto fijado»). `nav.jump-back` navega ahí como una navegación normal
(entra en el rastro, así que `nav.back` deshace el salto); sin punto, aviso
`msg-nav-no-jump-point`. Se guarda en la sesión (`SlotState.jump`, opcional,
`#[serde(default)]`).

**D6. Populares (Krusader Ctrl+Z).** Una sola lista GLOBAL de la sesión, no
por panel (Krusader también es global): `SessionBody.popular:
Vec<{path, visits}>`, tope 50. Cada navegación iniciada por el usuario
(`Trail::Record`, no `Replay` ni `Seed`) suma una visita. Al llenarse se
expulsa la de menos visitas y, en empate, la más antigua. `pane.popular` abre
la lista ordenada por visitas, con las mismas acciones que D2 salvo `clear`
(que vacía populares). El conteo vive en `norte_frontend::nav::Popular`; los
dos frontends lo llaman desde el mismo sitio donde llaman `History::record`.

**D7. Lista de un LADO (`pane.history-left/-right`).** Como
`pane.select-drive-left/-right`: el lado se resuelve al ABRIR y la lista
navega ESE panel aunque el foco esté en el otro. Título «Historia —
izquierda/derecha».

**D8. Filas compartidas.** `norte_frontend::nav::history_rows(&History,
current, filter) -> Vec<HistoryRow{path, mark: Current|Back|Forward}>` y
`popular_rows(&Popular, filter)`. `NavPopup` (TUI) y `Selector` (ventana)
dejan de construir filas cada uno: pintan y ejecutan. El test de paridad
compara las filas de las dos superficies para el mismo estado.

**D9. Ayuda.** Topic nuevo `history` (en/es): teclas por preset, qué es MRU
vs rastro, punto de salto, populares, acciones de la lista, qué se guarda y
que la TUI no ve los botones laterales del ratón. `panes.md` enlaza.

### Fuera

- Filtrado de «dirs de paso» de TC (`Alt+↓` vs `Alt+Shift+↓`): pide medir
  permanencia; sin caso real.
- Historia de ficheros vistos/editados (`Alt+F11` de Far): otra lista, otra
  fase.
- Historia global de todos los paneles a la vez: Krusader y mc son por panel;
  populares ya es la vista global.

### Tests

- `History`: cota configurable conserva el invariante (property test sobre
  secuencias de record/back/forward/remove); jump point; `history_rows` marca
  current/forward; filtro.
- `Popular`: expulsión por visitas y antigüedad; `Replay`/`Seed` no cuentan
  (verificado por mutación: quitar el guard pone el test rojo).
- Sesión: `jump` y `popular` redondean; un cuerpo viejo sin ellos carga.
- Presets: cada comando nuevo atado donde D1 dice; `el_conjunto_con_contador`
  no cambia (los nuevos van sin contador).
- Paridad: mismas filas y mismo destino en TUI y ventana para confirm,
  confirm-other, remove.
- Ventana: X1/X2 → `nav.back`/`nav.forward` (test del renderer).

---

## Fase 2 — Splash, procesos y retoques visibles

**Splash.** `[ui] splash = "off" | "brief" | "home"`, por defecto `brief`.
- Modelo compartido `norte_frontend::splash::SplashView { art: Vec<String>,
  version, revision, daemon: Embedded|Daemon|Connecting, sections:
  Vec<SplashSection> }`, con `SplashSection { title_key, rows: Vec<SplashRow
  { label, detail, command, arg } > }`.
- Las secciones salen de un REGISTRO (`SplashSource` trait): populares (fase
  1), favoritos, sesiones de perfil. Añadir una sección es implementar el trait
  y registrarla; la fase 3 deja que un plugin `panel` aporte una.
- `brief`: capa encima del primer pintado; se quita con cualquier tecla o
  cuando llega el primer listado + 1,2 s. NO retrasa el listado (<50 ms de
  arranque sigue siendo el objetivo). `home`: se queda hasta una tecla; `1..9`
  ejecuta la fila. Arte: una brújula ASCII con la N; la ventana usa el mismo
  arte en monoespaciada (una sola fuente de verdad).
- `ntc --no-splash` y `NORTE_NO_SPLASH` para scripts y tests. El asistente de
  primer arranque (ADR 0106) gana al splash.

**Procesos.** `[ui] processes_panel = "auto" | "manual"`, por defecto `auto`:
se abre al empezar una tarea y se cierra cuando la última termina y caduca
(los 10 s que ya tiene). Si el lector lo abrió o cerró a mano durante la
tarea, `auto` no le lleva la contraria hasta la siguiente. Velocidad y ETA
calculadas en cliente en `norte_frontend::processes` (media exponencial sobre
`bytes_done` y el tiempo entre snapshots; ETA solo con `bytes_total`). Barra en
la FILA de destino: `progress_for(&VPath) -> Option<f32>`; la TUI la pinta como
fondo proporcional del nombre, la ventana como gradiente.

**Retoques.**
- `[ui] dir_indicator = "auto" | "slash" | "none"`: `auto` quita la `/` cuando
  la columna de iconos está abierta (el icono ya clasifica).
- Pie de panel en la TUI: con el estilo del borde de SU panel (activo = acento)
  y un espacio a cada lado, no gris atenuado encima de la línea.
- Barra de teclas: `N Etiqueta` con espacio cuando la celda tiene ancho para
  número + espacio + 3 celdas de etiqueta; si no, como hoy.
- Ventana: panel sin foco atenuado (variable CSS `--inactive-dim`, del tema).

## Fase 3 — Kind `panel` y `StyledFrame`

- `norte_frontend::frame::StyledFrame { lines: Vec<Vec<Span>>, hits:
  Vec<Hit{row, col, width, command, arg}> }`. `Span` es el de los previews
  estilados (rol/fg/bg). Una función de pintura en la TUI y una en
  `render.ts`; un clic en un `Hit` va por el despacho de comandos normal
  (policy intacta, un plugin no ejecuta nada que el lector no pueda).
- WIT `norte:panel@0.1.0`, world `norte-panel`, importa `norte:location`:
  `render(kind, context{cols, rows, lang, cursor-name}, state: list<u8>,
  event: none | click(row, col) | key(command)) -> result<frame{lines, hits,
  state}, string>`. `state` son bytes opacos que el host guarda por hueco (cota
  64 KiB). Sin eventos de tecla crudos: el plugin recibe COMANDOS.
- Manifiesto `[[contributions.panel]] kind, title, min-cols, min-rows`. El
  `KindId` es `plugin:<id>:<kind>` (nunca choca con uno built-in).
  `KindRegistry::insert` desde `PluginInfo.panels` en los dos frontends.
- Proto: `plugin.panel_render` (cotas: 256 líneas, 256 spans/línea, 128 hits,
  4 MiB), `PluginInfo.panels`.
- Cuándo se repinta: al cambiar dir o cursor del panel con foco (con
  coalición), al redimensionar, tras un evento. Un plugin lento no bloquea: la
  última foto se queda con indicador.
- Demo oficial `plugins/git-panel`: rama (`.git/HEAD`), commit (`.git/logs/HEAD`
  última línea), últimos 10 movimientos del reflog, con `location-root-marker =
  ".git"`. Con `git-status` (columna) cubre la mejora 7.

## Fase 4 — Mapa de disco

- **Core, no plugin**: necesita una tarea cancelable (regla 3) sobre árboles
  de millones de entradas; el presupuesto de `norte:location` (4096 llamadas)
  la haría fallar en cualquier `$HOME`. La ADR lo dice.
- Proto `fs.dir_usage {path, depth: 1}` → tarea; resultado `DirUsage
  {children: [{name: bytes, kind, bytes, entries}], total, partial}`. Reutiliza
  el recorrido de `fs.dir_size`.
- Caché de tamaños por `VPath` en el frontend: el pie suma los dirs medidos y
  la columna Tamaño los muestra; se invalida con el watcher.
- `norte_frontend::treemap::squarify(children, cols, rows) -> StyledFrame`
  (colores por clase de fichero vía roles del tema; `Hit` → `nav.enter` del
  hijo). Slot kind built-in `disk-map`; comando `layout.disk-map`.

## Fase 5 — Imágenes de verdad en la TUI

- Sonda al arrancar (ya hay consulta de soporte de teclado): APC de kitty
  `a=q` + DA1. `[ui] images = "auto" | "kitty" | "blocks" | "off"`.
- Fuente: el kind `thumbnail` (PNG) que ya usa la ventana — un solo plugin
  sirve a las dos superficies. Kitty: `f=100`, placement por id en el rect del
  visor, borrado al repintar. Sin kitty: previewer `image-ansi` (bloques).
- Sixel fuera: codificar sixel pide un cuantizador; sin caso real.

## Fase 6 — Ir a cualquier sitio

- Comando `app.goto`. Tecla: `ctrl+g` en orthodox/cua/vim (libre en browse;
  se verifica en el plan), motivo en las cabeceras de los importados.
- `norte_frontend::goto::GotoSource` (trait): comandos del catálogo,
  historia + populares, favoritos, conexiones, índice semántico (asíncrono,
  llega después con su sección). Secciones con título; subsecuencia para las
  síncronas. Una fuente nueva = implementar el trait.
- Una ruta tecleada (`/`, `~`, `sftp://`) es una fila «ir a» directa.

## Fase 7 — Línea de tiempo del journal

- Proto `journal.list {before_seq, limit≤200, actor?}` → filas `{seq, ts_ms,
  actor_kind, actor_id, op, path, path_to, reversible, batch_id}`; solo actor
  humano autenticado. `journal.undo_after {seq}` → tarea que deshace en LIFO
  lo del USUARIO posterior a `seq` con las reglas de `undo.rs` (no-clobber,
  irreversibles se saltan y se cuentan).
- Slot kind built-in `timeline` como `StyledFrame`: un punto por entrada,
  color por actor, lote agrupado; `Hit` → seleccionar; `enter` → confirmar
  deshacer hasta ahí con el recuento previo. Revisión de seguridad
  obligatoria.

## Fase 8 — «Pregúntale a norte»: plan de organizar

- Kind nuevo `organizer` (`norte:organizer@0.1.0`), generaliza el renamer:
  propone `{current, proposed-rel}` donde `proposed-rel` puede llevar
  subdirectorios. El proveedor de IA sirve el mismo contrato por
  `ai.organize_plan {dir, instruction}`.
- Aplicar = `fs.create` de los dirs + `fs.move` por el pipeline del lote
  (journal, policy, un `batch_id`, deshacer entero).
- Revisión como diff de árbol en `StyledFrame` (nuevo en verde, movido con
  flecha), en las dos superficies.

## Fase 9 — Handoff entre TUI y ventana

- Solo en modo daemon. Comando `app.handoff`: el dueño vuelca la sesión
  (`session.put`), la suelta (`session.release`, nuevo) y lanza el otro
  frontend con `--attach`; ese la reclama en su `session.get`. Pestañas,
  directorios, cursor e historia ya viven en la sesión; las MARCAS no — se
  añaden a `SlotState.marks` (tope 4096, ids de fila, no índices).
- Desde la TUI remota (SSH) no hay ventana que abrir: el comando se anuncia
  no disponible con motivo.
