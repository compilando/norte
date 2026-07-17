# 0026 — Lua scripting embebido con mlua (M4)

- Estado: accepted
- Fecha: 2026-07-17
- Decisores: oscar, Claude
- Relacionado: spec §7.2 (mlua embebido, API síncrona de alto nivel sobre el
  protocolo, modelo validado por Yazi), §8 (paridad keymap ↔ palette ↔
  scripting ↔ agente); ADR 0022 (tabla de niveles D5: Scripts = Lua del
  usuario, sin sandbox, config propia — distinto de Plugins WASM y de
  Built-in); diseño de referencia
  `docs/superpowers/specs/2026-07-17-m4-lua-scripting-design.md`.

## Contexto y problema

La spec §7.2 pide un nivel de scripting Lua embebido: `init.lua` del usuario
define comandos invocables desde `keymap.toml` (`lua:nombre`) y un hook de
statusbar por pane, con una API síncrona de alto nivel sobre el protocolo (el
modelo que valida Yazi). ADR 0022 ya fija la tabla de niveles: **Plugins**
(WASM de terceros, sandbox WASI + capabilities) vs. **Scripts** (Lua del
usuario, SIN sandbox, permisos del usuario, es config propia) vs. **Built-in**
(comandos nativos del binario) — los tres nunca se mezclan (el anti-Krusader).
Este ADR cierra las decisiones de implementación del nivel Scripts: dónde vive
el módulo, qué dependencia de Lua se usa y con qué modelo de seguridad.

## Opciones consideradas

### A — Módulo en `norte-tui` + `mlua` (elegida)

El Lua vive en el frontend, en `crates/norte-tui/src/lua/`. Toda operación de
FS pasa por `Backend` (cliente del protocolo) → engine → journal/policy/undo:
es exactamente la regla 7 (frontends sin lógica de negocio) aplicada a un
disparador más — un `init.lua` que invoca comandos del protocolo, igual que
una tecla del keymap o la agenda de un agente MCP.

- ✅ Cero circularidad: el driver Lua necesita tipos del `App` (snapshot de
  pane, selección, cwd) que hoy solo existen en `norte-tui`.
- ✅ Coherente con ADR 0022 D5 (Scripts = capa de config del frontend, no del
  core) y con la regla 7.
- ➖ Si en M5 la GUI necesita el mismo scripting, el módulo se extrae a un
  crate `norte-lua` compartido — API pensada para eso desde ya (frontera
  `LuaHost` clara), pero no se prematuriza ahora.

### B — Crate `norte-lua` aparte

- ✅ Reusable desde el día uno por cualquier frontend (TUI y la futura GUI
  M5).
- ❌ Hoy mismo genera circularidad: la API Lua necesita los tipos de estado
  del `App` de `norte-tui` (snapshot de pane, selección) para las funciones
  `norte.pane.*`; sacarlos a un crate compartido antes de que exista un
  segundo consumidor es especular sin necesidad (regla 8, sin deps/crates sin
  justificación real). Extraíble en v2 si la GUI M5 lo pide.

### C — Lua en el core (`norte-core`)

- ❌ Contradice la spec §7.2 directamente: el scripting es una capa de
  **frontend** (comandos que disparan protocolo), no una feature del daemon.
  Metería scripting de usuario, sin sandbox, dentro del proceso AGPL que
  media policy/journal para TODOS los clientes (incluidos agentes) — mezcla
  de niveles que ADR 0022 D5 prohíbe explícitamente. Descartada.

## Decisión

**A.** Nuevo módulo `crates/norte-tui/src/lua/` sobre `mlua` (Lua 5.4
vendorizado). Modelo de seguridad, explícito y documentado en el propio
módulo:

- **SIN sandbox.** El script corre con los permisos del usuario — es config
  (como un `.bashrc`), no software de terceros. Stdlib completa disponible
  (`io.*`, `os.*`, `load`…, sin restringir). Se documenta con nitidez la
  frontera: `norte.fs.*` pasa por `Backend` → engine (journal + policy +
  undo, deja rastro); `io.*`/`os.*` crudos NO dejan rastro — el usuario los
  usa bajo su propio riesgo, igual que un shell.
- **Capas de confianza.** `/etc/norte/init.lua` y `~/.config/norte/init.lua`
  cargan siempre (los controla el propio usuario). La capa de PROYECTO
  (`./.norte/init.lua`, que puede venir con un repo ajeno) solo se evalúa tras
  aprobación TOFU explícita por `(path canónico, sha256)`, persistida en el
  state dir: si el fichero cambia, se re-pregunta. Mismo patrón que el modal
  TOFU de host keys (#45) y el gobierno humano de plugins (M4-P3, ADR 0022
  D4). Motivo: Lua sin sandbox + auto-ejecución de un repo clonado es RCE
  silencioso — exactamente la clase de agujero que norte cierra en el resto
  del árbol.
- **Ejecución async sin bloquear la UI.** `mlua` con feature `async`: los
  futures de mlua son `!Send`, así que las corrutinas se pollean INLINE en el
  loop principal del TUI — jamás `tokio::spawn` (rompería `Send`). Un comando
  Lua en vuelo cede el control al loop entre pasos, igual que cualquier otra
  Task (regla 3: cancelación vía `CancellationToken`, chequeada en el punto
  donde la corrutina espera al engine).

Dependencia: `mlua = { version = "0.10", features = ["lua54", "vendored",
"async", "serialize"] }`, declarada en `[workspace.dependencies]` y
consumida por `norte-tui`. `lua54` fija la versión del lenguaje (paridad con
Yazi); `vendored` compila el C de Lua 5.4 embebido (sin depender de un
`liblua` del sistema, cero variabilidad de versión/parche entre máquinas);
`serialize` habilita el paso de tablas Lua ↔ `serde` (la API `norte.*`
serializa `Entry`/snapshots sin conversión manual campo a campo).

## Consecuencias

- ➕ Dependencia estructural nueva `mlua` (MIT). El C de Lua 5.4 vendorizado
  compila dentro del build de `norte-tui`; su `unsafe` es del propio Lua/FFI
  de mlua, no de código nuestro (regla 5 no aplica a dependencias de
  terceros, solo al código de norte). `cargo deny check licenses` verde
  (verificado en este mismo commit).
- ➕ El módulo `lua/` queda como scaffold puro en este task (submódulos
  comentados: `api`, `driver`, `fs`, `statusbar`, `trust` — cada uno se activa
  al implementarse en su task correspondiente del plan); el crate compila hoy
  sin ninguna funcionalidad Lua expuesta todavía.
- ➖ Modelo de seguridad "sin sandbox" exige disciplina de documentación: todo
  punto de entrada público del módulo debe dejar claro qué SÍ pasa por
  journal/policy/undo y qué NO (rustdoc, no solo este ADR).
- ➖ Reforzar en el driver (task futura) que las corrutinas mlua nunca cruzan
  a un `tokio::spawn`: un desliz ahí sería un panic en runtime (futures
  `!Send` movidos a un executor multi-hilo), no un error de compilación
  detectado tarde si se envuelven en un tipo que borre el `!Send` sin querer.
