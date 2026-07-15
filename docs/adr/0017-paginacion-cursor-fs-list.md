# 0017 — Paginación por cursor de `fs.list`: stream retenido por conexión

- Estado: accepted
- Fecha: 2026-07-13
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: spec §11 ("paginación por cursor + streaming incremental; el
  TUI pinta las primeras 100 entradas en <16 ms aunque el dir tenga 500k") y
  §12 (presupuesto: listado 100k < 200 ms hasta primer render); ADR 0004
  §"Paginación futura de `fs.list`" (cláusula de compatibilidad); ADR 0011
  (daemon, límites anti-DoS); issue #27 (254 ms medidos vs 200 ms). Plan M2
  fase 7 (decisión de kickoff: #27 se ataca con la paginación — un diseño para
  #27 y los listados enormes de object storage). ADR 0016 (provider S3, misma
  fase).

## Contexto y problema

`fs.list` devuelve hoy el listado COMPLETO en una respuesta
(`FsListResult { entries }`): el handler del daemon drena el `EntryStream` del
provider, `Backend::list` entrega un `Vec<Entry>` y el TUI no pinta hasta
tenerlo entero. Con 100k entradas son 254 ms hasta el primer render (#27);
con un bucket S3 de 500k sería peor y además un frame de respuesta gigante.
El trait `Provider::list` ya es un stream perezoso — el problema es
exclusivamente del wire y de los consumidores.

Restricción heredada (ADR 0004): un core con paginación DEBE seguir
devolviendo el listado completo a clientes que no envíen cursor — jamás
truncar en silencio a un N-1.

## Opciones consideradas

- **O1 — cursor stateless (re-list + skip N)**: sin estado en el daemon, pero
  O(n²) sobre listados grandes e INCORRECTO bajo mutación concurrente (el
  skip por índice pierde/duplica entradas si el dir cambió entre páginas).
- **O2 — token nativo del provider** (ContinuationToken de S3, posición de
  readdir…): exigiría ensanchar `Provider::list` con tokens que
  local/sftp/mem/archive no tienen — superficie nueva en TODOS los providers
  para servir a uno.
- **O3 — stream retenido server-side**: el cursor es el id opaco de un
  `EntryStream` VIVO que el daemon retiene entre llamadas, con estado
  POR-CONEXIÓN. `EntryStream` ya es perezoso y cancelable soltándolo (regla
  3, contrato testeado); para S3 el lister de opendal arrastra su
  ContinuationToken por debajo, gratis. Coherente con los límites por
  conexión ya existentes (ADR 0011). ELEGIDA.
- **O4 — método `fs.list_close`**: cierre explícito de cursores. Rechazada:
  superficie innecesaria a esta escala — LRU + TTL + muerte con la conexión
  bastan (un TUI navega 1-2 listados vivos).

## Decisión

- **Wire (proto 0.7.0 → 0.8.0, aditivo)**: `FsListParams` gana
  `limit: Option<u32>` y `cursor: Option<String>`; `FsListResult` gana
  `next_cursor: Option<String>` (todos `#[serde(default)]`).
  - Sin cursor y sin limit → drenado completo, `next_cursor: null` (cláusula
    ADR 0004, con test explícito que la pinnea).
  - `limit: Some(0)` → `INVALID_PARAMS`. Límite superior `FS_LIST_MAX_PAGE =
    10_000` (recortar no es error, patrón `FS_READ_MAX_CHUNK`).
  - Continuación: `cursor: Some` (+ `path` SIEMPRE requerido; si no coincide
    con el registrado → `INVALID_PARAMS`). `limit: null` en continuación =
    drena el resto.
  - Cursor desconocido/expirado → **`Error::CursorExpired`** (variante nueva;
    el cliente reinicia el listado; un cliente 0.7 degradaría a `Unknown`
    pero JAMÁS la ve — no envía cursores).
  - Página final posiblemente vacía (`entries: []` + `next_cursor: null` es
    fin normal, sin lookahead). Guard en el CLIENTE: página vacía CON
    `next_cursor: Some` → `Error::Internal` (jamás bucle infinito contra un
    server roto; precedente del guard de `fs.read`).
  - Un `Err` del stream a mitad de página responde el error y DESCARTA el
    listado retenido.
- **Estado del daemon**: el `initialized: bool` por conexión pasa a
  `ConnState { initialized, listings: HashMap<u64, OpenListing>,
  next_listing_id }`, local de `serve_connection` (drop en todos los caminos
  de salida = los streams mueren con la conexión). `MAX_OPEN_LISTINGS = 8`
  por conexión (LRU: abrir el 9º expulsa al más viejo; drop = cancelación
  cooperativa). TTL `DaemonConfig.listing_ttl` (default 120 s, configurable =
  testeable): barrido perezoso en cada `fs.list` + brazo de `select!` activo
  solo si hay listings — una conexión viva-pero-muda no retiene streams (ni
  hilos blocking parkeados del productor local) para siempre. Token =
  contador decimal por conexión serializado a `String`, documentado como
  opaco (lookup por-conexión: un cursor ajeno no existe; el peer ya está
  autenticado por uid, ADR 0011).
- **Trait `Provider`: NO cambia.** La paginación vive entera en el daemon.
- **`Backend`**: método nuevo `list_stream(dir) -> Result<EntryStream>` —
  embebido = passthrough del engine; remoto = primera página EAGER
  (`LIST_PAGE = 1000`, paridad de errores `NotFound`/`TypeMismatch` en el
  `Result`, no como primer item) + `try_unfold` para las siguientes.
  `Backend::list` conserva su firma (drenado de `list_stream`) → CLI intacta,
  y el `ls` remoto de un dir gigante deja de arriesgar el `CALL_TIMEOUT` y el
  frame monstruoso.
- **TUI — streaming incremental**: `cd` pinta las primeras `FIRST_PAGE = 100`
  entradas (ordenadas) con indicador `loading` y un drenador en background
  rellena por batches coalescidos (4096 entradas o 100 ms), con re-sort
  completo por batch y re-anclaje del cursor por path del seleccionado. Un
  listado incompleto SIEMPRE se marca en la UI (jamás silencioso). Un cd
  nuevo dropea el canal → el drenador y el stream mueren (regla 3). Re-sort
  completo y NO merge incremental: 2-3 re-sorts tras el primer paint según el
  bench; si el bench demuestra que rompe el frame budget, el merge con claves
  persistidas queda apuntado como issue (no se optimiza a ciegas).
- **Los 100k stat de vfs-local NO se tocan aquí**: el presupuesto de spec §12
  es "hasta primer render" y con la primera página el primer render ya no los
  espera. `kind` desde `d_type` + size/mtime lazy = issue aparte (coordinada
  con el `bytes_total` del walk del copy engine, que consume `entry.size`).
- **Cursores NO sobreviven a la reconexión** (estado por conexión): el
  cliente relista — el TUI ya lo hace al reconectar.
- **Bench** (`presupuestos.rs`): `cien_mil_hasta_primer_render` se redefine
  al camino nuevo (primera página); el drenado completo se CONSERVA como vara
  de regresión y métrica de la issue de d_type. Cierre de #27 con números en
  la issue.

## Consecuencias

Positivas:

- Primer render de un dir de 100k pasa de 254 ms a ~el coste de 100 entradas
  (spec §11 cumplida de sobra); S3/500k hereda el mismo camino.
- Cero cambios en el trait ni en los providers; el copy engine no se toca.
- Frames de respuesta acotados (≤ `FS_LIST_MAX_PAGE` entradas).
- Cliente 0.7 sigue funcionando sin cambios (cláusula ADR 0004 con test).

Negativas / deuda asumida:

- Estado nuevo en el daemon (streams retenidos): acotado por 8/conexión + TTL
  + muerte con la conexión + idle-shutdown. El peor caso de hilos blocking
  parkeados (256 conn × 8 = 2048 productores de vfs-local en `blocking_send` >
  pool 512) lo cierra un **tope GLOBAL** (`GLOBAL_MAX_LISTINGS = 256`, bien bajo
  el pool): por encima, un listado nuevo NO se retiene — se drena entero en
  línea (libera el hilo al instante) y degrada a listado-completo, jamás agota
  el pool ni trunca. Contabilidad RAII (guard en `OpenListing`). Tuning fino
  (max_blocking_threads, TTL) + test de connection-drop = issues de deuda.
- El fill del TUI re-ordena por LOTE (`FILL_BATCH = 4096`): un dir de 100k son
  ~24 re-sorts de tamaño creciente durante el relleno (no bloquea el primer
  render; el drenado completo = 250 ms de vara). El merge incremental con
  claves persistidas es la optimización diferida (issue).
- Un listado paginado NO es una foto consistente (el dir puede mutar entre
  páginas) — inherente a cualquier paginación sobre un FS vivo; mismo
  contrato que hoy (el orden del provider tampoco garantiza nada).
- El TUI re-ordena por batch durante el fill (frames algo más caros mientras
  llega un dir gigante), medido por bench con plan B apuntado.
- `Error::CursorExpired` es superficie nueva de taxonomía que los frontends
  deben tratar (reiniciar el listado).

## Cierre de #27 (fase 10c M2)

Revisado en el cierre de M2: la paginación por cursor (implementada en fase
7f) mata #27 por diseño. El primer render de un dir de 100k ya NO drena las
100k — pinta `FIRST_PAGE = 100` entradas, así que su coste pasa de los 254 ms
medidos al coste de 100 entradas, holgadamente bajo el presupuesto de 200 ms
(spec §12). El bench `primera_pagina()` de `norte-tui/benches/presupuestos.rs`
mide ESE camino (no el drenado completo, que se conserva como vara de
regresión). #27 queda CERRADO; la deuda residual (merge incremental con claves
persistidas durante el fill, tuning de max_blocking_threads/TTL) vive en sus
propias issues, no en #27.
