# Gate de lectura para agentes (cierra #80) — diseño

- Fecha: 2026-07-18
- Estado: **IMPLEMENTADO** (2026-07-18, inline en `daemon/server.rs`
  `read_gate` + los 4 handlers; `handle_fs_search` lo reusa; tests en
  `daemon.rs` y `mcp/bridge.rs`; #80 cerrada). Se ejecutó inline (un helper
  + 5 call-sites + tests), no por plan de subagentes, por tamaño.
- Contexto: issue #80 (ALTA, LOAD-BEARING). M3 gateó las MUTACIONES
  (fs.copy/move/delete) para `Actor::Agent`; las LECTURAS quedaron abiertas
  por simetría con el humano. `fs.search` (live search T4) rompió esa apuesta
  (lectura recursiva de contenido) y añadió un gate de lectura SOLO para
  search (`ScopeRegistry::covers_read`). Este proyecto extiende ese gate a
  `fs.list`/`fs.read`/`fs.stat`/`fs.capabilities` — el confinamiento de
  lectura del agente que M3 no cerró. Cero cambio de protocolo.

## Objetivo

Un `Actor::Agent` solo lee (list/read/stat/capabilities) bajo un scope vivo
concedido a su sesión; `User` sin restricción. Igual criterio que las
mutaciones de M3 y que `fs.search` de T4.

## Decisiones (con el porqué)

1. **op-independiente (reusa `covers_read`), NO `PolicyOp::Read`.** Cualquier
   scope vivo que cubra la raíz concede lectura, sin importar sus ops. Leer
   es estrictamente menos que cualquier mutación, y copy/move/delete YA
   implican leer (listas para borrar, lees para copiar). Reusa la pieza que
   `fs.search` ya usa — una fuente, cero cambio del modelo de grants
   (`request_scope`/`grant_scope`/`OpSet` intactos). **Residual aceptado:**
   un grant de solo-`mkdir` bajo `/x` concede lectura de `/x` (mkdir no
   implica leer contenido ajeno); caso raro, no una escalada real (no expone
   nada que el grant no implique ya poder crear). Se documenta; si algún día
   se quiere least-privilege puro (write-only-no-read), es `PolicyOp::Read`,
   fuera de alcance.
2. **Duro / default-deny, no configurable.** Un agente sin scope que cubra la
   ruta NO lee, igual que no muta. Coherente con «la IA es un ciudadano, no
   un dueño» (spec §1.5) y simétrico con M3. Cambia el comportamiento de
   sesiones de agente existentes (deben pedir scope antes de listar/leer) —
   el flujo `request_scope` → grant humano → op ya existe y el puente MCP lo
   guía. Un opt-in dejaría el hueco abierto-por-defecto: el confinamiento de
   `fs.search` seguiría siendo teatro salvo activación manual.
3. **Cero cambio de protocolo.** `Error::PolicyDenied{rule}` ya existe; esto
   es solo comportamiento del daemon. Sin bump, sin goldens nuevos,
   protocol-guardian no obligatorio.

## Componentes

### `read_gate` — helper único (daemon o `ScopedPolicy`)

Extrae la lógica hoy inline en `handle_fs_search` a una función reusada:

```
fn read_gate(actor: &Actor, path: &VPath, scopes: &ScopeRegistry) -> Result<(), Error>
```
- `User` → `Ok(())`.
- `Agent{session}` → `scopes.covers_read(session, path, Instant::now())`:
  `Within` → `Ok(())`; `OutOfScope` → `PolicyDenied{rule: "out-of-scope"}`;
  `Expired` → `PolicyDenied{rule: "scope-expired"}` (vía
  `DenyReason::rule_id()`, vocabulario cerrado — categoría gruesa, jamás la
  regla concreta ni el path; sin loguear el vpath).
- `_` (Plugin futuro) → `PolicyDenied` (default-deny, `match` con wildcard
  deliberado como en `handle_fs_search`).

`handle_fs_search` pasa a llamar `read_gate` (cierra la duplicación que el
security-reviewer de T4 dejó anotada).

### Los cuatro handlers gatean ANTES de tocar el engine

Todos tienen el actor a mano (verificado):
- `handle_fs_list` (server.rs:949, tiene `conn`) — gate sobre `p.path`. Solo
  el arranque; la continuación por cursor es del mismo path ya validado
  (`listing.path != p.path` ya se comprueba).
- `FS_STAT` (`dispatch_fs_task`, tiene `actor`) — gate sobre `p.path`.
- `FS_READ` (`dispatch_task_family`, ya rosca `actor` para TASK_LIST) — gate
  sobre `p.path`.
- `FS_CAPABILITIES` (`dispatch_task_family`) — gate sobre `p.path` (revela
  existencia/tipo del provider en esa ruta; por consistencia).

`task.list` NO se toca: ya filtra por actor (`may_observe`, #66); no es
lectura de FS.

## Errores

- Agente fuera de scope → `PolicyDenied{rule}` (mismo wire que la mutación
  denegada y que `fs.search`). El puente MCP (bridge.rs:351) YA lo traduce a
  «denied by policy ({rule}). If out-of-scope, call request_scope…» — cero
  cambio en el puente.
- `User` jamás recibe `PolicyDenied` por lectura.

## Impacto

- **Puente MCP: cero cambio de código.** Reenvía 1:1; el flujo del agente
  (request_scope → grant → list/read) ya funciona, ahora aplica a lecturas.
- **TUI/CLI humanos: sin cambio** (son `Actor::User`).
- **Criterio de salida de M3:** pasa a ser cierto («confinamiento de lectura
  del agente»); actualizar la nota que hoy dice que depende de #80.

## Tests

1. Unit de `read_gate`: User pasa; Agent within → Ok; out-of-scope → Denied;
   expired → Denied{scope-expired}; actor no-User/no-Agent → Denied.
2. Daemon: `agente_sin_scope_no_{lista,lee,statea,capabilities}` (los cuatro
   → PolicyDenied) + `agente_con_scope_lee` (grant por wire que cubre la raíz
   → Ok en los cuatro) + `humano_lee_sin_scope` (User sin restricción).
   `handle_fs_search` sigue verde reusando `read_gate` (sin regresión).
3. E2E puente MCP: un agente sin scope recibe el PolicyDenied accionable en
   `list_dir`/`read_file`; tras `request_scope` + grant, lee.

## Fuera de alcance

`PolicyOp::Read` / least-privilege write-only-no-read (el residual mkdr se
acepta documentado); gate de lectura para plugins (no llegan por el
handshake del socket hoy — el default-deny los cubre estructuralmente);
cualquier cambio de protocolo.

## Criterio de salida

Un agente sin scope recibe `PolicyDenied` en list/read/stat/capabilities y
en search; con un scope que cubre la raíz, los cinco funcionan; un humano
(User) lee sin restricción; `just ci` verde; #80 cerrada y la nota del
criterio M3 actualizada.
