# Live search (Alt+F7: nombre + contenido, pane virtual) — diseño

- Fecha: 2026-07-18
- Estado: **IMPLEMENTADO** (2026-07-18, plan
  `docs/superpowers/plans/2026-07-18-live-search.md`, T1..T7). Se implementó
  DESPUÉS de `2026-07-18-navegacion-tc-design.md`, como estaba previsto.
- Contexto: spec del producto §17.1 modo (a) — live search sobre VFS:
  nombre por glob/regex y contenido tipo grep, streaming como Task
  cancelable, consciente de encodings; resultados como «pane virtual»
  operable estilo TC. El modo (b) indexado (FTS5/semántica, norte-index)
  queda explícitamente FUERA — proyecto futuro.

## Objetivo

`Alt+F7` busca por nombre y/o contenido bajo el subtree del pane activo
(cualquier provider: local, sftp, S3, zip), los hits llegan en streaming a
un pane virtual desde el que se opera (F3/F5/F8, Enter = ir al fichero).

## Decisiones

1. **Enfoque a — Task + notificaciones** (frente a b polling por cursor y
   c solo-embebido): `fs.search` devuelve `{task_id}`; los hits viajan en
   notificación `search.hits` por LOTES; el fin es el estado terminal de la
   Task. Reusa entera la infra existente: `task.cancel`, `task.progress`,
   broadcast de notifs del daemon y la bomba del `RemoteBackend`. Paridad
   embebido/daemon (regla 7).
2. **Read-only, sin journal** (regla 4 no aplica: no muta). Sí es Task con
   token chequeado en el inner loop del walker (regla 3) + test de
   cancelación limpia.
3. **Contenido consciente de encodings — transcodificar la AGUJA, no el
   pajar** (§17.1): la aguja literal se codifica a los encodings candidatos
   (UTF-8 y las codificaciones de `norte-encoding`/detector: Latin-1/1252,
   Shift-JIS, …) y se busca bytes-contra-bytes (memmem por chunks con
   solape = len(aguja_max)−1). Binarios (detector NUL-density existente) se
   SALTAN para contenido (cuentan solo para nombre). Regex de contenido:
   solo sobre ficheros que el detector clasifique texto, decodificando por
   chunks con solape — más caro, documentado.
4. **Pane virtual = listing normal** para el resto del TUI: las entradas
   son `Entry` reales (VPath completo); selección/F5/F8/F3 funcionan sin
   camino especial (feed-to-listbox). Enter sobre un hit = cd al padre +
   cursor sobre el fichero. Navegar fuera (cd, Ctrl+R al cwd previo)
   abandona el modo virtual; la Task, si sigue viva, se cancela.

## Wire (norte-proto — bump minor + goldens + protocol-guardian OBLIGADO)

- `fs.search` (request): params
  `{root: VPath, name_glob: Option<String>, name_regex: Option<String>,
    content: Option<String>, content_regex: Option<String>,
    case_sensitive: bool (default false), max_hits: Option<u32>}` →
  result `{task_id}`. Al menos un criterio obligatorio (los cuatro `None` =
  `INVALID_PARAMS`). Glob y regex EXCLUYENTES por eje (ambos = inválido).
  El matching de nombre es sobre el nombre decodificado lossy en NFC
  (consistente con `unicode_compare` de la spec §"case-insensitive");
  documentado en el rustdoc del método.
- `search.hits` (notificación server→client):
  `{task_id, entries: Vec<Entry>, matches: Option<Vec<MatchInfo>>}` con
  `MatchInfo { line: Option<u64>, preview: Option<String> }` alineado 1:1
  con entries cuando la búsqueda es de contenido (preview = línea del
  match decodificada lossy + RECORTADA server-side, tope fijo; el TUI la
  sanea con `detail_for_bar` igualmente). Lotes con coalescing (tope de
  entradas por notif, p.ej. 256; flush por intervalo).
- `TaskKind::Search` nuevo (el enum ya tolera Unknown N-1, ADR 0004).
- Solo a la CONEXIÓN que lanzó la búsqueda (no broadcast: los hits de tu
  búsqueda no son de otros frontends; mismo criterio direccional que
  approval_required→humanos).
- Policy/actor: `fs.search` es lectura — gate de policy con `PolicyOp` de
  lectura para agentes (scope subtree, igual que fs.list). El puente MCP
  gana la tool `search` DESPUÉS (deuda anotada, no en este proyecto).

## Core (norte-core)

- `engine::search(params, actor) -> TaskRef`: walker iterativo (pila
  explícita, sin recursión — profundidad hostil no revienta el stack)
  sobre `Provider::list`, orden BFS, symlinks NO seguidos en v1
  (documentado; los ciclos de symlink quedan imposibles), token en cada
  entrada, `max_hits` corta con estado terminal `Completed` + flag
  `truncated` en el progress final.
- Matcher de nombre: glob (crate `globset` — dep nueva justificada:
  la usa ripgrep, mantenida, sin unsafe; alternativa evaluada: glob a mano
  = bugs seguros) o regex (crate `regex` — verificar si ya está en el
  árbol; compilada UNA vez, con `size_limit` anti-ReDoS).
- Matcher de contenido: `Provider::read` streaming por chunks (64 KiB) con
  solape; agujas pre-codificadas (memchr/memmem ya en el árbol vía deps —
  verificar); regex sobre texto decodificado por chunks. Tope de bytes por
  fichero configurable en params v2 (v1: sin tope, el token cancela).
- Progress: `task.progress` reutilizado (scanned/hits como los bytes de
  copy — encajar en los campos existentes de TaskProgress; si no encajan,
  extensión de proto documentada con el guardian).
- Hits → observer/canal hacia el daemon (embebido: canal directo al
  Backend). El daemon serializa `search.hits` SOLO al peer dueño.

## TUI

- `Alt+F7` (`pane.search` en COMMANDS + presets): diálogo modal con campos
  nombre / contenido, toggles regex y case (Tab entre campos, Enter lanza,
  Esc cancela). Root = cwd del pane activo (mostrado, no editable v1).
- Pane virtual: reemplaza el listing del pane activo; título/status =
  «búsqueda: N hits (buscando…/completa/truncada/cancelada)». Los lotes
  llegan por la bomba de notifs → `extend_listing` (mismo camino que el
  fill paginado). Esc = cancela la Task (y conserva los hits llegados);
  Ctrl+R o cd = salir del modo.
- Nombres/paths hostiles: render con `display_name`/`path_display` como
  cualquier listing (mask + badge ya existentes). Preview de contenido:
  `detail_for_bar`.
- Claves Fluent `search-*` en en/es.

## Errores

- Regex inválida / glob inválido / cero criterios: error del método
  (`INVALID_PARAMS` con detalle), el diálogo lo muestra sin cerrarse.
- Errores por-entrada durante el walk (permiso denegado en un subdir,
  fichero ilegible): NO abortan la búsqueda; contador de «saltados» en el
  progress (mismo espíritu que skipped del indexado de archives).
- Task Failed (root desaparece, conexión caída): pane virtual muestra la
  categoría; los hits ya recibidos se conservan.

## Tests

1. Proto: goldens de `fs.search` + `search.hits` + `TaskKind::Search`;
   fuzz de framing sigue verde.
2. Core unit: matcher de nombre (glob/regex, NFC case-insensitive, corpus
   hostil — NFD casa con aguja NFC); matcher de contenido (aguja «año» en
   fixture Latin-1 Y UTF-8 del corpus de encodings; binario se salta;
   solape de chunks caza aguja partida en frontera).
3. Core integración: walk sobre MemProvider anidado con límites;
   cancelación limpia a mitad de walk (regla 3); max_hits trunca;
   errores por-entrada no abortan; permiso denegado contado.
4. Daemon: round-trip embebido y remoto (hits solo al dueño; otro peer NO
   los ve); reconexión a mitad = Task muere con la conexión (dueño único).
5. TUI: pane virtual recibe lotes y opera (F5 desde resultados copia el
   VPath real — test tipo lua_e2e con backend_mem); Enter = cd al padre +
   cursor; Esc cancela y conserva.
6. E2E criterio de salida.

## Fuera de alcance

Búsqueda indexada (FTS5/semántica — norte-index, proyecto propio);
filtros fecha/tamaño (v2 sobre los mismos params); root editable en el
diálogo; tool `search` del puente MCP (deuda anotada al cerrar); seguir
symlinks; búsqueda DENTRO de archives anidados más allá de lo que el
provider composite ya lista.

## Criterio de salida

Alt+F7 con «año» como contenido bajo un árbol con ficheros UTF-8 y
Latin-1 encuentra ambos y los muestra en streaming; F5 desde los
resultados copia al otro pane; Esc a mitad cancela limpio conservando lo
llegado; lo mismo contra el daemon y sobre sftp; `just ci` verde con
goldens nuevos.

Verificado por el E2E `crates/norte-tui/tests/search_e2e.rs` (T7,
`Backend::Embedded` con `MemProvider`): `criterio_de_salida_año` (UTF-8 +
Latin-1 + fichero UTF-8 anidado, F5-equivalente byte-exacto),
`cancel_conserva_lo_llegado` (cancelación a mitad con `MemProvider::faults`
latency, hits parciales conservados), `nombre_hostil` (bytes crudos
`0xFF 0xFE` intactos por `name_glob`), `ejes_name_regex_y_content_regex`
(los cuatro ejes de criterio cubiertos entre los cuatro tests). El
round-trip real contra sftp/daemon queda cubierto por los tests de T4/T5
(`backend_remote.rs`, suite del daemon), no repetido aquí.

## Desviaciones de la implementación (2026-07-18)

- **`FsTaskResult` reusado**: `fs.search` no tiene un struct de resultado
  propio — el `{task_id}` de siempre (methods.rs), documentado en el
  rustdoc de `FS_SEARCH` en vez de un tipo nuevo. Cero superficie de wire
  extra para algo que ya existía.
- **Sin flag `truncated` en el wire**: el diseño original hablaba de un
  `truncated` en el progress final; en la implementación el cliente
  INFIERE truncado comparando `hits == max_hits` (no hay campo dedicado).
  La TUI usa un tope propio `SEARCH_MAX_HITS = 10_000` por defecto y no
  expone el campo en el diálogo v1 (root fijo = cwd, sin edición de
  límites — ya estaba fuera de alcance para el root; se extiende al tope
  de hits).
- **Saltados plegados en `entries_done`**: no hay un contador separado de
  «entradas saltadas» (binarios para contenido, errores por-entrada de
  list/read) — todos cuentan como `entries_done` del progress estándar,
  igual que las entradas que sí matchean. El diseño original sugería un
  contador dedicado tipo «skipped del indexado de archives»; se decidió
  reusar el campo existente para no ampliar `TaskProgress`.
- **Gate de scope de lectura para agentes**: `fs.search` gatea con
  `ScopeRegistry::covers_read` (`daemon/server.rs`), el mismo criterio que
  `fs.list`/`fs.read`/`fs.stat`/`fs.capabilities`. **#80 CERRADA** (2026-07-18,
  spec `2026-07-18-gate-lectura-agentes-design.md`): el helper `read_gate`
  es la fuente única y los 4 reads + search lo consultan. El confinamiento
  de lectura del agente por scope ya es REAL — un agente sin scope no lee
  nada, igual que no muta.
- **Preview de contenido NO surface en el pane v1**: el pane virtual es
  una vista PLANA tipo listing normal (feed-to-listbox) — `MatchInfo`
  (línea + preview recortado) viaja por el wire y se calcula server-side,
  pero la TUI no lo pinta en una columna del pane; solo el status bar
  agregado (`search-status-*`). Mostrar el preview por fila queda anotado
  como **#81**.
- **`StreamDecoder` para UTF-16 y multi-chunk**: el matcher de contenido
  original hablaba de «decodificar por chunks con solape»; la
  implementación final usa `norte_encoding::StreamDecoder` (nuevo tipo del
  crate de encoding, no existía en el recon del plan) para sostener el
  estado de decodificación entre chunks — necesario para que un match en
  la línea N de un fichero UTF-16 (BOM en cabeza, pares de bytes) no se
  pierda ni se desalinee al cortar por `\n` crudo en la frontera de chunk
  (regresión pinneada en `engine_search.rs::utf16_match_en_linea_posterior_le_y_be`).
- **`ContentNeedle::for_encoding` tras `detect`**: además del modo
  multi-encoding (aguja pre-codificada a TODOS los candidatos de
  `reload_cycle()`), hay un modo dirigido `for_encoding(text, case, enc)`
  que usa el `Encoding` que YA detectó el fichero — mata falsos positivos
  cross-encoding (p. ej. un CJK en UTF-8 con un byte líder que por
  casualidad coincide con la aguja Latin-1 corta; pinneado en
  `contenido_encoding_aware_tres_ficheros`). No estaba en el diseño
  original, que solo mencionaba el modo multi-encoding.
- **`is_under` re-verificado en el walker**: defensa en profundidad —
  aunque el `Provider::list` debería devolver solo descendientes del path
  pedido, `run_walk` (`search.rs`) vuelve a comprobar `is_under(confine,
  entry.path)` y descarta cualquier entrada que un provider (con bug o
  malicioso) devuelva fuera del subtree pedido. Pinneado en
  `walk_ignora_entradas_fuera_del_root_aunque_el_provider_las_liste`
  (review de seguridad T3).
- **Ciclo de vida del route de hits remoto con gracia**: el diseño decía
  «se retira al ver terminal para ese id»; la implementación añade
  `SEARCH_ROUTE_GRACE = Duration::from_millis(500)` (best-effort, no
  garantía estructural) para cubrir la carrera entre el último lote de
  `search.hits` y la notificación terminal de `task.progress` llegando en
  cualquier orden por el mismo socket. Un cierre estructural (p. ej. un
  marcador explícito de fin de stream en vez de gracia por tiempo) queda
  como mejora futura, no bloqueante para el criterio de salida.
- **`Alt+F7` sin colisión en los tres presets**: confirmado por grep
  contra los TOML de `keymap_presets/` antes de bindear — F7 estaba libre
  (el mkdir clásico de Total Commander no existe aún en norte).

Referencia: issues **#80** (gate de lectura fs.list/fs.read pendiente —
`fs.search` sola no basta para confinamiento real de agentes) y **#81**
(preview de contenido de un hit no se muestra en el pane, solo viaja por
wire).
