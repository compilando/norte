# M3-3 — Policy engine + scopes — Design

- Fecha: 2026-07-14
- Estado: diseño aprobado (pendiente plan + implementación)
- Relacionado: spec §5 (la IA es ciudadano, no dueño: allow/ask/deny + audit),
  §10 (scopes + policy engine + `policy.*` + `policy.approval_required`), §14
  (threat model agentes), regla dura 9 (agentes/plugins nunca tocan el FS
  directo; todo por core → policy). M3-1 (journal/actor), M3-2 (undo — deuda:
  actor ejecutor + undo bajo policy, que M3-3 cierra). Criterio de salida M3:
  "Claude Code gestiona un dir bajo policy ask, con undo de sesión completa".

## Contexto

M3-1/M3-2 dieron journal + undo, pero toda mutación corre como `User` con
`AllowAll` implícito. M3-3 mete el enforcement: un agente opera dentro de un
SCOPE (rutas + ops + TTL) y cada mutación se evalúa contra una POLICY
(allow/ask/deny) ANTES de ejecutarse. El `ask` empuja una aprobación
interactiva al frontend. Es el sub-proyecto que hace del criterio de salida algo
real: "un dir bajo policy ask".

## Decisiones (brainstorming)

1. **Alcance = todo el vertical** (Q1): motor + scopes + gating embebido **y** el
   round-trip proto/daemon/TUI. Se descompone en **3a** (core embebido) y **3b**
   (proto + daemon + TUI).
2. **Modelo de evaluación** (Q2): `User` = allow siempre (el humano no se
   sandboxea). `Agent`/`Plugin`: primero la FRONTERA de scope (op/ruta fuera del
   scope activo → `Deny` duro); dentro, las reglas de `policy.toml` (primera
   coincidencia) deciden allow/ask/deny; sin regla aplicable → **default deny
   (fail-closed)**.
3. **Granularidad + Ask** (Q3): una evaluación POR OPERACIÓN (no por nodo). La
   llamada del engine (que el agente hace por MCP) AWAIT-ea la decisión: `Ask`
   suspende hasta que un frontend apruebe/deniegue (TTL → deny).
4. **Actor threading**: las ops mutantes del engine ganan `actor: &Actor`;
   alimenta la policy Y el `TaskCtx.actor` del journal. Cierra la deuda M2 de
   M3-2 (compensaciones con actor real).
5. **Scope = subtree-prefix** (contención de directorio), NO globs — cubre "un
   dir" sin dep nueva; globs = deuda.
6. **`PolicyOp` propio** (no `TaskKind`), con detalle (delete mode).
7. **El undo pasa por el gate** igual que las mutaciones (cierra deuda M3 de
   M3-2).
8. **Sin policy configurada = `AllowAll`** (embebido/humano de hoy); con policy,
   fail-closed para agentes.

## Arquitectura

### Costura y actor threading (3a)

Las ops mutantes del `Engine` (`copy_with`, `move_with`, `delete_with`) y
`undo_session` ganan un parámetro `actor: &Actor` (o un contexto de sesión que
lo lleve). El actor:
- se pasa al `PolicyGate` para evaluar,
- se fija en `TaskCtx.actor` (hoy default `User` en el scheduler) → el journal
  registra el actor real.

Call-sites actuales (CLI/TUI/tests) pasan `Actor::User` explícito. El
`Scheduler::submit` gana el actor (o el `TaskCtx` se construye con él).

### Tipos (crate `norte-core`, módulo `policy`)

```rust
/// Operación evaluable por la policy (más detalle que TaskKind).
pub enum PolicyOp {
    Copy,
    Move,
    Delete { mode: DeleteMode }, // Permanent vs Trash
    Mkdir,
}

/// Veredicto del motor.
pub enum Decision {
    Allow,
    Ask(ApprovalRequest),
    Deny(DenyReason),
}

pub enum DenyReason {
    OutOfScope,     // ruta/op fuera del scope del agente
    ScopeExpired,
    PolicyRule,     // una regla deny de policy.toml
    NoRule,         // fail-closed: ninguna regla aplicó
    NotApproved,    // Ask denegado o TTL vencido (en el gate)
}

/// Un scope concedido a una sesión de agente.
pub struct Scope {
    pub roots: Vec<VPath>,      // contención por subtree-prefix
    pub ops: OpSet,             // qué PolicyOp permite pedir
    pub expires_at: std::time::Instant,
}
```

`Error::PolicyDenied { reason }` en `norte-proto` (variante nueva → cambio de
wire aditivo en 3b; en 3a puede vivir como error interno del core hasta el
bump). Nota: mapear a una categoría existente si se prefiere evitar wire en 3a.

### Evaluación

```rust
pub trait PolicyGate: Send + Sync {
    fn evaluate(&self, actor: &Actor, op: PolicyOp, paths: &[&VPath]) -> Decision;
}
```

- `User` → `Allow`.
- `Agent { session }` / `Plugin { id }`:
  1. **Scope boundary**: para CADA path de la op, ¿existe un scope activo (no
     expirado) de esta sesión cuyo `roots` contenga el path por prefijo Y cuyo
     `ops` incluya `op`? Si algún path no lo cumple → `Deny(OutOfScope)` (o
     `ScopeExpired`).
  2. **Reglas** `policy.toml`: primera regla que matchea (op + prefijo de ruta +
     scheme + actor-kind) → su acción (`allow`/`ask`/`deny`).
  3. Sin regla → `Deny(NoRule)` (fail-closed).

Los scopes activos por sesión viven en un `ScopeRegistry` (in-memory,
thread-safe) que el motor consulta. En 3a se siembran directamente; en 3b los
puebla el flujo `request_scope`/`grant_scope`.

### `policy.toml`

En `config_dir()/policy.toml` (respeta `NORTE_CONFIG_DIR`/XDG). Lista ordenada
de reglas; primera coincidencia gana. Ejemplo:

```toml
[[rule]]
op = "delete"           # copy|move|delete|mkdir; ausente = cualquiera
recursive = true        # opcional (delete)
path_prefix = "file:///" # prefijo VPath (wire); ausente = cualquiera
scheme = "sftp"         # opcional
actor = "agent"         # user|agent|plugin; ausente = cualquiera
action = "ask"          # allow|ask|deny
```

Parsing con `toml` (ya en el árbol) + `serde` con `deny_unknown_fields`. Rutas
como prefijo de `VPath::to_wire` (ASCII, line/parse-safe). Condiciones size/ext/
hora = deuda (YAGNI M3-3).

### Gating (PRE-efecto) + Ask

El `Engine` gana `Arc<dyn PolicyGate>` (default `AllowAll`) y
`Arc<dyn ApprovalResolver>` (default `DenyAll`). Cada op mutante, ANTES de
`submit`:

```rust
match self.policy.evaluate(actor, op, &paths) {
    Decision::Allow => { /* procede al submit */ }
    Decision::Deny(reason) => return Err(policy_denied(reason)),
    Decision::Ask(req) => match self.approvals.request(req).await {
        ApprovalOutcome::Approved => { /* procede */ }
        ApprovalOutcome::Denied | ApprovalOutcome::TimedOut =>
            return Err(policy_denied(DenyReason::NotApproved)),
    },
}
```

```rust
pub trait ApprovalResolver: Send + Sync {
    async fn request(&self, req: ApprovalRequest) -> ApprovalOutcome;
}
```

- Default `DenyAll` (headless fail-closed) y `AllowAll` para la policy → el
  engine embebido de hoy (humano) sigue funcionando sin config.
- La op sigue siendo Task tras aprobarse (progreso/cancelación intactos). El
  `Ask` suspende la LLAMADA del engine, no una Task (la Task nace ya aprobada).
- `undo_session` evalúa `PolicyOp` por cada `revert_entry` (o una vez por la
  sesión de undo — decisión de plan; probablemente por-entrada porque cada
  reversa es una mutación distinta). Cierra la deuda M3 de M3-2.

### Proto round-trip (3b)

Bump de `PROTOCOL_VERSION` (0.10.0 → 0.11.0, aditivo). Añade:

- **Notificación** `policy.approval_required` (server→client):
  `{ approval_id, session, op, paths (redactadas para spans/logs si llevan
  userinfo), ttl_ms }`.
- **Método** `policy.decide { approval_id, decision: "approve"|"deny" }`.
- **Método** `policy.pending` → lista de aprobaciones pendientes (resync tras
  reconexión).
- **Método** `policy.request_scope { roots, ops, ttl_ms }` (agente pide) →
  pendiente de concesión.
- **Método** `policy.grant_scope { request_id }` (humano concede) → puebla el
  `ScopeRegistry`.
- `Error::PolicyDenied { reason }` en la taxonomía de `RpcError.data`.

Daemon: un `ApprovalResolver` que, ante `Ask`, registra la aprobación pendiente,
difunde `policy.approval_required` a los clientes inicializados, y await-ea el
`policy.decide` correspondiente con TTL. `ScopeRegistry` por conexión/sesión.
protocol-guardian OBLIGATORIO + golden + ventana N/N-1.

TUI: modal de aprobación (preview de la op: kind + rutas + actor) con
accept/deny; y UX de scope grant. Strings por Fluent (`i18n/`).

## Testing

- **3a (embebido, unit + integración)**:
  - `PolicyGate::evaluate`: matriz — User→allow; Agent fuera de scope→deny;
    dentro sin regla→deny(NoRule); reglas allow/ask/deny; scope expirado→deny;
    op no concedida→deny.
  - `policy.toml` parsing: reglas válidas, `deny_unknown_fields`, primera-gana.
  - Gating en el engine: Allow procede; Deny→`PolicyDenied` sin tocar el FS; Ask
    con resolver callback Approved→procede, Denied/TTL→`PolicyDenied`.
  - Actor threading: una op como `Agent` deja la entrada de journal con ese
    actor (cierra deuda M2).
  - Undo bajo policy: `undo_session` de un agente fuera de scope→deny.
  - Cancelación: sin regresión (Ask que se cancela).
- **3b (proto + daemon + E2E)**:
  - Golden de los tipos `policy.*` + ventana de versión.
  - Daemon: cliente-agente simulado sobre el socket pide una op → recibe
    `policy.approval_required` en otro cliente → `policy.decide approve` →
    la op procede; `deny`/TTL → `PolicyDenied`.
  - Scope grant: `request_scope` → `grant_scope` → op dentro procede, fuera
    deniega.
  - Redacción: rutas con userinfo no se filtran a logs (regla 10).

## No-objetivos (YAGNI en M3-3)

- Globs en scopes (subtree-prefix basta para "un dir") → deuda.
- Condiciones de regla size/ext/hora → deuda.
- norte-mcp server real (tools MCP, `request_scope` desde el agente vía MCP) →
  M3-4 (aquí el "agente" es un cliente del socket simulado en tests).
- Rate limits por sesión de agente (§14) → deuda.
- Persistencia de scopes concedidos entre reinicios (viven en memoria; TTL).
- Audit export de decisiones → M3-5 (el journal ya capta las mutaciones que SÍ
  ocurren; las denegadas son deuda de audit).

## Deuda / riesgos

- El gate PRE-submit del engine no debe establecer conexiones remotas dirigidas
  por rutas del journal/agent sin control (heredado de la deuda M3-2): en el
  undo, resolver providers sólo de los ya registrados o pasar por scope primero.
- `Error::PolicyDenied` es wire (3b): en 3a se puede mantener interno y exponer
  en 3b con el bump — evita un wire-change a medias.
- La suspensión del `Ask` bloquea la llamada del engine: en el daemon, el outbox
  acotado y el TTL evitan que un agente cuelgue recursos indefinidamente
  (coherente con los límites anti-DoS del daemon existente).

## Decomposición (para el plan)

- **3a** — `norte-core::policy` (`PolicyOp`/`Decision`/`Scope`/`ScopeRegistry`/
  `PolicyGate`/`AllowAll` + `policy.toml` parsing) + `ApprovalResolver`
  (`DenyAll` default) + actor threading en engine/scheduler + gating PRE-efecto
  en copy/move/delete/undo. Tests embebidos. Sin wire.
- **3b** — proto `policy.*` + `policy.approval_required` + `Error::PolicyDenied`
  (bump 0.11.0, golden, protocol-guardian) + daemon approval router +
  `ScopeRegistry` por sesión + `request_scope`/`grant_scope` + TUI modal + E2E.
