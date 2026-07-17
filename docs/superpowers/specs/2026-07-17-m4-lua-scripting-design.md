# M4 — Lua scripting (comandos + statusbar) — diseño

- Fecha: 2026-07-17
- Estado: aprobado (oscar, conversación 2026-07-17)
- Contexto: spec §7.2 (mlua, API síncrona sobre el protocolo, permisos del
  usuario sin sandbox), §8 (paridad keymap ↔ palette ↔ scripting ↔ agente),
  ADR 0022 (tabla de niveles: Scripts = Lua del usuario, no sandbox, config
  propia).

## Objetivo

`init.lua` del usuario define **comandos** invocables desde `keymap.toml`
(`lua:nombre`) y un **hook de statusbar** por pane. Cubre keybindings
programáticos y automatizaciones (los dos usos de más valor de §7.2);
linemode/columnas custom queda para v2.

## Decisiones (con el porqué)

1. **Alcance v1: comandos + statusbar.** Linemode se ejecuta N veces por
   render — el más caro y difícil de acertar a la primera; fuera.
2. **Capas y trust:** `/etc/norte/init.lua` y `~/.config/norte/init.lua`
   cargan siempre (los controla el usuario). `./.norte/init.lua` (viene con
   un repo, potencialmente AJENO) solo tras aprobación explícita estilo
   TOFU/direnv: modal con sha256 del fichero, decisión persistida por
   `(path canónico, hash)` en el state dir — fichero cambiado = re-pregunta.
   Sin decisión = no se carga, con aviso en barra. Mismo patrón que el modal
   TOFU de host keys (#45) y el gobierno humano de plugins (M4-P3). Motivo:
   Lua corre SIN sandbox con los permisos del usuario; auto-ejecutar el de un
   repo clonado sería RCE silencioso — la clase de agujero que norte cierra.
3. **Stdlib completa** (`io`, `os`, `load`…): la spec es explícita («no
   sandboxed; es config»). Documentación NÍTIDA de la distinción: `norte.fs.*`
   pasa por el engine (journal + undo + policy); `io.*`/`os.*` crudos no
   dejan rastro, como un shell. El trust-gate del punto 2 cubre el vector del
   repo ajeno.
4. **Arquitectura A — módulo `norte-tui/src/lua/`** con `mlua` (lua54,
   vendored, features `async` + `serialize`). El Lua vive en el frontend y
   TODA operación de FS va vía `Backend` (cliente del protocolo) → engine →
   journal/policy. Regla 7 intacta: es config del usuario disparando
   comandos del protocolo, igual que una tecla. Alternativas descartadas:
   crate `norte-lua` aparte (circularidad con tipos del App; extraíble en v2
   si la GUI M5 lo pide) y Lua en el core (contradice §7.2 y mete scripting
   de usuario en el daemon).
5. **`mlua` = dep estructural + decisión de seguridad → ADR nuevo** (regla 8
   y convención ADR). MIT; `unsafe` interno es del dep (regla 5 no aplica a
   terceros); `cargo deny` a verificar en el plan.

## Componentes

### `lua/mod.rs` — `LuaHost`

Estado Lua + registro. Se construye al arranque y se **reconstruye entero**
en hot-reload de config (comandos re-registrados; jamás estado a medias). Un
comando en vuelo sobrevive al reload con su estado VIEJO (la corrutina retiene
el estado anterior hasta terminar; el registro nuevo aplica a invocaciones
nuevas).

- `LuaHost::load(layers, trust) -> (LuaHost, Vec<LuaWarning>)` — evalúa los
  `init.lua` presentes en orden de capas (sistema → usuario → proyecto-si-
  confiado). Error de evaluación de una capa: se reporta (barra, por
  categoría) y se sigue con las demás — un init.lua roto no tumba la TUI ni
  bloquea las otras capas.
- `commands() -> Vec<String>` — para detección de conflictos y palette
  futura.
- `invoke(name, ctx, token) -> CommandRun` — arranca la corrutina.
- `statusbar(snapshot) -> Option<String>` — hook síncrono, ver abajo.

### `lua/api.rs` — la tabla `norte`

Inyectada como global al evaluar cada `init.lua`.

- `norte.command(nombre, fn)` — registra. Nombre validado `[a-z0-9._-]{1,64}`
  (mismo espíritu que `agent_session`); duplicado en la MISMA capa = error de
  carga; capa posterior PISA a la anterior (precedencia de config, ADR 0007,
  con warning).
- `norte.fs.list(path) -> {entries}`, `stat(path) -> entry`,
  `copy(src, dst)`, `move(src, dst)`, `delete(path)` (= trash; permanente NO
  expuesto en v1), `mkdir(path)`. Mutaciones esperan al estado terminal de la
  Task y devuelven `true` o `nil, err`; convención Lua estándar.
- `norte.pane.cwd()`, `other_cwd()`, `selection() -> {paths}`,
  `current() -> path|nil` — snapshot del momento de la llamada.
- `norte.ui.message(s)` — barra, saneado con `detail_for_bar` (#73).
- `norte.ui.statusbar(fn)` — registra el hook (último gana).

**Paths = byte strings de Lua** en TODA la API (entrada y salida). Lua los
maneja nativos (strings de 8 bits); encaja con VPath sin suponer UTF-8
(regla 1). Conversión: bytes → `VPath` vía la ruta local del pane actual
(paths relativos se resuelven contra `cwd()` del pane con foco); inválido =
`nil, err`, jamás panic ni lossy silencioso.

### `lua/driver.rs` — ejecución async

Cada `invoke` crea una corrutina mlua-async sobre el runtime tokio del TUI:
el script VE una API síncrona (`copy` «bloquea» hasta terminal) pero solo la
corrutina espera — la UI sigue viva. Serialización: **un comando en vuelo**;
invocaciones nuevas se encolan FIFO (tope pequeño, p.ej. 8; llena = se
descarta con aviso). Sin reentrada.

**Cancelación (regla 3):** Esc durante un comando Lua → se cancela el
`CancellationToken` del `invoke`; el driver racea la corrutina contra el
token; las Tasks del engine en vuelo lanzadas por el comando se cancelan vía
`task.cancel`; la corrutina muere con error `cancelled`. Test de cancelación
limpia obligatorio.

**Timeout duro** además del token (p.ej. 5 min, configurable en v2): un
script colgado en C (stdlib `os.execute`) no responde al token — el driver
abandona la espera (la corrutina se dropea al reconstruir el host) y avisa.

### `lua/statusbar.rs` — hook de barra

- Síncrono y PURO: recibe una tabla snapshot (`cwd`, `selected` (nº),
  `selected_bytes`, `entries` (nº), `tasks` (nº vivas)) y devuelve string.
- Presupuesto por instrucciones con `set_hook` de mlua (p.ej. 50k
  instrucciones): excedido → hook DESHABILITADO hasta el próximo reload +
  error por barra (categoría Fluent). Llamar API async (`norte.fs.*`) desde
  el hook = error inmediato, mismo destino.
- Resultado cacheado por cambio de snapshot, no por frame. Salida por
  `detail_for_bar` (mask + tope): el string del script no inyecta
  bidi/controles en la barra.

### `lua/trust.rs` — trust store del init.lua de proyecto

- Fichero `lua-trust.toml` en el state dir (`$XDG_STATE_HOME/norte`, 0600),
  entradas `(path canónico, sha256, decisión, fecha)`.
- Al detectar `./.norte/init.lua` sin entrada que case (path+hash):
  `Modal::TrustLuaInit` (cola de modales existente) con path saneado + hash
  abreviado; `y` = confiar y cargar, `n`/`Esc` = denegar (persistido: no
  re-preguntar hasta que el hash cambie). Enter NO aprueba (convención del
  modal de approvals).
- El hash se calcula sobre los BYTES leídos una única vez; lo aprobado es
  EXACTAMENTE lo evaluado (sin TOCTOU read-approve-reread).

## Errores

- Error Lua (carga o comando): barra = clave Fluent (`err-lua-load`,
  `err-lua-command`, `err-lua-statusbar`, `err-lua-cancelled`…) + detalle por
  `detail_for_bar`; traceback completo NO va a la barra (v1: se descarta
  tras el detalle; si M4 trae log de frontend, irá ahí).
- Errores del protocolo dentro de `norte.fs.*`: al script llegan como
  `nil, categoria` (la misma `error_category` del #20 — el script puede
  mostrarla o decidir).

## Tests

1. Unit: registro (duplicado misma capa = error; capa posterior pisa),
   validación de nombres, `commands()`.
2. Trust: hash distinto = re-pregunta; denegado persiste; aprobado carga;
   TOCTOU (lo evaluado = lo hasheado).
3. Driver: cancelación limpia (comando con Task de copia en vuelo → Esc →
   Task cancelada + corrutina muerta + destino limpio/`.norte-partial`);
   cola FIFO con tope; timeout duro.
4. Statusbar: presupuesto excedido = deshabilitado sin tumbar la TUI; API
   async desde el hook = error; salida hostil (bidi) enmascarada.
5. Encoding: round-trip de bytes no-UTF8 por la API (`list` de un dir con
   nombre `0xFF` → byte string → `copy` con ese path funciona) — fixture del
   corpus si aparece hueco nuevo.
6. E2E: `init.lua` real que registra un comando que copia la selección al
   otro pane y lo renombra, sobre MemProvider, invocado por keymap.
7. Snapshot UI del modal de trust.

## Fuera de alcance v1

Linemode/columnas custom; palette (Ctrl+P); API de eventos/hooks
(before/after op — eso es el nivel WASM); `norte.fs` permanente-delete;
require/paquetes Lua de terceros; timeout configurable; log de frontend.

## Criterio de salida

Con un `init.lua` de usuario: una tecla dispara un comando Lua que lista,
copia y renombra vía engine (visible en journal y deshecho con undo), la
statusbar custom pinta, Esc cancela limpio, y un `./.norte/init.lua` de un
repo ajeno NO se ejecuta sin aprobación explícita.
