# M3-3b — Policy round-trip (proto + daemon + TUI + E2E) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Exponer el enforcement de policy por el protocolo: una conexión puede actuar como sesión de agente; sus mutaciones se evalúan server-side; una regla `ask` empuja `policy.approval_required` a los frontends y suspende la op hasta `policy.decide` (TTL→deny); un agente pide scope con `policy.request_scope` y un humano lo concede con `policy.grant_scope`.

**Architecture:** Proto 0.11.0 aditivo: `InitializeParams.agent_session` (liga la conexión a un `Actor::Agent` server-side — no declarable como `User`, cierra la deuda M3 de 3a) + métodos `policy.*` + notificación `policy.approval_required`. `Error::PolicyDenied{rule}` YA existe (no bump por él). El daemon: `ConnState.actor`, dispatch de `fs.copy/move/delete` con `*_with_as(actor)`, un `ScopeRegistry` compartido, y un `DaemonApprovalResolver` (implementa `ApprovalResolver`) que registra la aprobación pendiente, difunde `policy.approval_required` y await-ea el `policy.decide` correspondiente por un `oneshot` con TTL. El engine del daemon se construye con `with_policy(ScopedPolicy(scopes, cfg), resolver)`. TUI: modal de aprobación.

**Tech Stack:** Rust, JSON-RPC (proto::wire), `tokio::sync::oneshot`, `nextest`. Reviewers: `protocol-guardian` OBLIGATORIO (Task 1), `security-reviewer` OBLIGATORIO (Task 4: actor-binding, approval routing, TTL), `rust-reviewer` por task.

**Nota de threat model:** el daemon es UDS same-uid (§14). La policy es un guardarraíl para agentes que COOPERAN (vía norte-mcp, M3-4), no un sandbox contra código local arbitrario del mismo uid (que ya puede todo). Un cliente que declara `agent_session` ES un agente y se sandboxea; los frontends humanos no lo declaran (`User`).

---

## Task 1: proto 0.11.0 — `agent_session` + `policy.*` + `approval_required`

**Files:** `crates/norte-proto/src/methods.rs`, `crates/norte-proto/src/error.rs` (solo doc), tests `types.rs`/`golden_types.rs`.

- [ ] **Step 1: Test rojo** — en `types.rs`: roundtrip de `agent_session` (Some/None), de `PolicyApprovalRequired`/`PolicyDecideParams`/`RequestScopeParams`/`GrantScopeParams`/`PolicyPendingResult`, y ventana 0.11/0.10.

- [ ] **Step 2** — `InitializeParams` gana:
```rust
    /// Si presente, la conexión actúa como SESIÓN DE AGENTE con este id: sus
    /// mutaciones se evalúan por policy (M3-3). Ausente = frontend humano
    /// (`User`, sin sandbox). El servidor liga el actor a la conexión; el
    /// cliente no puede declararse `User` por otra vía.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<String>,
```

- [ ] **Step 3** — método consts + tipos (en `methods.rs`):
```rust
pub const POLICY_REQUEST_SCOPE: &str = "policy.request_scope";
pub const POLICY_GRANT_SCOPE: &str = "policy.grant_scope";
pub const POLICY_DECIDE: &str = "policy.decide";
pub const POLICY_PENDING: &str = "policy.pending";
pub const POLICY_APPROVAL_REQUIRED: &str = "policy.approval_required"; // notif server→client
```
Tipos (todos `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]`):
```rust
pub struct RequestScopeParams { pub session: String, pub roots: Vec<VPath>, pub ops: Vec<String>, pub ttl_ms: u64 }
pub struct RequestScopeResult { pub request_id: u64 }
pub struct GrantScopeParams { pub request_id: u64 }
pub struct GrantScopeResult {}
pub struct PolicyApprovalRequired { pub approval_id: u64, pub session: Option<String>, pub op: String, pub paths: Vec<String>, pub ttl_ms: u64 }
pub struct PolicyDecideParams { pub approval_id: u64, pub approve: bool }
pub struct PolicyDecideResult {}
pub struct PendingApproval { pub approval_id: u64, pub session: Option<String>, pub op: String, pub paths: Vec<String> }
pub struct PolicyPendingResult { pub pending: Vec<PendingApproval> }
```
(Rutas como wire redactado con `span_path` si llevan userinfo — regla 10.)

- [ ] **Step 4** — bump `PROTOCOL_VERSION` a `"0.11.0"` + rustdoc de la versión. Actualiza ventana en `types.rs` (0.11/0.10/reject 0.9), `golden_types.rs` (assert 0.11.0), daemon N-1 test (client 0.10.x). Golden de los tipos nuevos si el corpus los exige.

- [ ] **Step 5: Verde** `cargo nextest run -p norte-proto`. **protocol-guardian** (aditividad, ventana, `agent_session` optional compatible, redacción de paths). Commit.

---

## Task 2: daemon — actor por conexión + dispatch como agente

**Files:** `crates/norte-core/src/daemon/server.rs`.

- [ ] **Step 1** — `ConnState` gana `actor: crate::journal::Actor` (default `User`); en el handler de `initialize`, si `params.agent_session = Some(s)` → `conn.actor = Actor::Agent { session: s }`.
- [ ] **Step 2** — `dispatch_fs_task` recibe `&ConnState` (o el actor); `fs.copy/move/delete` usan `copy_with_as/move_with_as/delete_with_as(&p..., conn.actor.clone())`. Propaga la firma (dispatch → dispatch_fs_task).
- [ ] **Step 3** — mapear `Error::PolicyDenied` en `RpcError::from` (verifica que ya viaja con su categoría; si el gate de 3a devolvía `PermissionDenied`, cámbialo a `PolicyDenied{rule}` en `engine.gate` con el `DenyReason` como `rule` — y actualiza los tests de 3a que aseveraban `PermissionDenied`).
- [ ] **Step 4** — test daemon: un cliente que `initialize` con `agent_session` y sin scope → `fs.copy` responde `PolicyDenied`. Un cliente humano (sin agent_session) copia OK. Commit (rust-reviewer).

---

## Task 3: daemon — `ScopeRegistry` + request_scope/grant_scope

**Files:** `server.rs` (+ `Shared` gana `scopes: ScopeRegistry` y `pending_scope_reqs`).

- [ ] **Step 1** — `Shared` gana `scopes: ScopeRegistry` (el MISMO que el `ScopedPolicy` del engine — inyectado al construir) y un mapa `pending_scope: Mutex<HashMap<u64, PendingScope>>` + contador.
- [ ] **Step 2** — handler `policy.request_scope`: registra la petición pendiente, devuelve `request_id`. `policy.grant_scope`: busca la petición, `scopes.grant(session, Scope{roots, ops, expires_at = now + ttl})`, la quita de pendientes.
- [ ] **Step 3** — test: agente `request_scope` → (sin grant) `fs.copy` dentro sigue deny; humano `grant_scope` → `fs.copy` dentro procede, fuera deny. Commit (rust + security).

---

## Task 4: daemon — approval router (Ask round-trip)

**Files:** `server.rs` (+ `DaemonApprovalResolver`), wiring del engine.

- [ ] **Step 1** — `DaemonApprovalResolver { pending: Arc<Mutex<HashMap<u64, oneshot::Sender<bool>>>>, broadcast: <fn/Sender>, next_id, ttl }` implementa `ApprovalResolver::request`: asigna `approval_id`, crea `oneshot`, guarda el sender, difunde `policy.approval_required`, `tokio::time::timeout(ttl, rx)`: Ok(true)→Approved, Ok(false)→Denied, Err(timeout)→TimedOut (limpia el pending).
- [ ] **Step 2** — El engine del daemon se construye con `with_policy(ScopedPolicy(scopes, cfg), Arc::new(resolver))`. El `pending`/`broadcast` se crean ANTES y se comparten entre el resolver (en el engine) y el `Shared` del daemon (para que `policy.decide` acceda).
- [ ] **Step 3** — handler `policy.decide`: busca el `approval_id` en `pending`, envía `approve` por el `oneshot`. `policy.pending`: lista los pendientes (resync).
- [ ] **Step 4** — E2E: cliente-agente con scope + regla `ask` hace `fs.copy` → otro cliente recibe `policy.approval_required` → `policy.decide approve` → la copia procede; `deny`/TTL → `PolicyDenied`. Commit. **security-reviewer OBLIGATORIO** (actor no spoofeable, TTL real, pending sin fuga, outbox acotado no cuelga workers).

---

## Task 5: TUI — modal de aprobación

**Files:** `crates/norte-tui/src/*`, `crates/norte-tui/i18n/`.

- [ ] **Step 1** — el TUI en modo daemon se suscribe a `policy.approval_required`; muestra un modal (op + rutas + sesión) con accept/deny → `policy.decide`. Strings por Fluent (`t!`). Test de UI del modal (patrón `tests/modal.rs`). Commit.

---

## Task 6: cierre

- [ ] `just ci` verde. Memoria: **M3-3b COMPLETA** (+ **M3-3 CERRADO**). SIGUIENTE M3-4 (norte-mcp). Deuda 3b restante (m4-m7 de 3a si no se cerraron: namespace scope key, revoke, symlink escape fixture). Commit.

---

## Riesgos / verificar

1. Chicken-egg del resolver: `pending`/`broadcast` compartidos se crean antes del engine y del daemon. Revisar el orden de construcción en el binario `norte daemon run` y en el test.
2. `Error::PolicyDenied` cambia el error del gate (3a devolvía `PermissionDenied`): migrar los asserts de `tests/policy.rs`.
3. `agent_session` optional + `skip_serializing_if` → un cliente 0.10 no lo envía; un server 0.11 lo trata como `None` (humano). Compatible.
4. El Ask suspende el future del handler de conexión (no el pool de 4 workers) — el TTL evita cuelgue; documentar el límite por-conexión.
5. Redacción de rutas en `policy.approval_required` (regla 10): reusar `span_path`.
6. m4 (namespace scope key por `(actor_kind,id)`) y m7 (`revoke`) de 3a: cerrarlos aquí si el ScopeRegistry se toca, o dejar deuda explícita.
