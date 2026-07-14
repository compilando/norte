# M3-3a — Policy engine core + scopes + gating (embebido) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Un motor de policy en `norte-core` que, dado (actor, operación, rutas), decide `Allow|Ask|Deny` combinando frontera de scope (para agentes) + reglas `policy.toml`; el engine lo consulta PRE-efecto en cada mutación y suspende en `Ask` vía un `ApprovalResolver` inyectable; el actor real se threadea hasta el journal.

**Architecture:** Módulo `policy` con tipos puros + `ScopedPolicy` (evaluación) + `ScopeRegistry` (grants en memoria por sesión). `Engine` gana `Arc<dyn PolicyGate>` (default `AllowAll`) y `Arc<dyn ApprovalResolver>` (default `DenyAll`); las mutaciones pasan por `gate(actor, op, paths)` antes de `submit`. `Scheduler::submit` y las ops mutantes ganan el `actor`; los métodos públicos actuales delegan como `User` (cero churn en CLI/TUI), y variantes `*_as(actor)` sirven el camino agéntico. Sin cambio de wire (denegación = `Error::PermissionDenied` en 3a; `PolicyDenied` llega en 3b).

**Tech Stack:** Rust, `toml` + `serde` (ya en el árbol), `async_trait`, `tokio`, `nextest`. Sin deps nuevas (scope = subtree-prefix sobre `VPath::segments`, sin globset).

**Reviewers:** `security-reviewer` OBLIGATORIO (enforcement de agentes, fail-closed, confused-deputy). `rust-reviewer` en cada task. `encoding-auditor` en la contención de rutas (bytes de `VPath`). No toca `norte-proto` → sin protocol-guardian en 3a.

---

## File Structure

- **`crates/norte-core/src/policy.rs`** (nuevo) — `PolicyOp`, `Decision`, `DenyReason`, `Scope`, `OpSet`, `ScopeRegistry`, `PolicyGate` trait, `AllowAll`, `ScopedPolicy`, `is_under`.
- **`crates/norte-core/src/policy/config.rs`** (nuevo, o submódulo inline) — `PolicyConfig`/`Rule` (`policy.toml`), `load`.
- **`crates/norte-core/src/approval.rs`** (nuevo) — `ApprovalRequest`, `ApprovalOutcome`, `ApprovalResolver` trait, `DenyAll`.
- **`crates/norte-core/src/scheduler.rs`** — `submit` gana `actor`; `TaskCtx.actor` desde él.
- **`crates/norte-core/src/engine.rs`** — campos `policy`/`approvals`, `with_policy`, ops `*_as(actor)` + gating, `undo_session` gatea por entrada.
- **`crates/norte-core/src/lib.rs`** — `mod policy; mod approval;` + reexports.
- **Tests**: `crates/norte-core/tests/policy.rs` (nuevo), unit en los módulos.

---

## Task 1: módulo `policy` — tipos + contención + `ScopedPolicy` (frontera de scope)

**Files:**
- Create: `crates/norte-core/src/policy.rs`
- Modify: `crates/norte-core/src/lib.rs`
- Test: `crates/norte-core/src/policy.rs` (mod `tests`)

- [ ] **Step 1: Test rojo — evaluación de frontera de scope**

Crea `crates/norte-core/src/policy.rs` con los tipos y estos tests al final (fallará a compilar hasta implementar):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Actor;
    use norte_proto::{DeleteMode, VPath};

    fn vp(w: &str) -> VPath {
        VPath::parse(w).expect("wire")
    }

    #[test]
    fn user_is_always_allowed() {
        let reg = ScopeRegistry::new();
        let pol = ScopedPolicy::new(reg, PolicyConfig::default());
        let d = pol.evaluate(&Actor::User, PolicyOp::Delete { mode: DeleteMode::Permanent }, &[&vp("file:///x")]);
        assert!(matches!(d, Decision::Allow));
    }

    #[test]
    fn agent_out_of_scope_is_denied() {
        let reg = ScopeRegistry::new();
        let pol = ScopedPolicy::new(reg, PolicyConfig::default());
        let agent = Actor::Agent { session: "s1".into() };
        // Sin scope concedido → todo fuera.
        let d = pol.evaluate(&agent, PolicyOp::Copy, &[&vp("file:///a/x")]);
        assert!(matches!(d, Decision::Deny(DenyReason::OutOfScope)));
    }

    #[test]
    fn agent_in_scope_without_rule_is_denied_fail_closed() {
        let reg = ScopeRegistry::new();
        reg.grant("s1", Scope::forever(vec![vp("file:///a")], OpSet::all()));
        let pol = ScopedPolicy::new(reg, PolicyConfig::default()); // sin reglas
        let agent = Actor::Agent { session: "s1".into() };
        let d = pol.evaluate(&agent, PolicyOp::Copy, &[&vp("file:///a/x")]);
        assert!(matches!(d, Decision::Deny(DenyReason::NoRule)), "fail-closed sin regla");
    }

    #[test]
    fn scope_containment_is_byte_exact_prefix() {
        // file:///a NO contiene file:///ab (prefijo de bytes engañoso).
        assert!(is_under(&vp("file:///a"), &vp("file:///a/x")));
        assert!(is_under(&vp("file:///a"), &vp("file:///a")));
        assert!(!is_under(&vp("file:///a"), &vp("file:///ab")));
        assert!(!is_under(&vp("file:///a"), &vp("file:///b/x")));
        // Distinto scheme/authority nunca contiene.
        assert!(!is_under(&vp("file:///a"), &vp("mem:///a/x")));
    }
}
```

- [ ] **Step 2: Verifica que falla**

Run: `cargo nextest run -p norte-core policy::tests::scope_containment_is_byte_exact_prefix`
Expected: FAIL de compilación.

- [ ] **Step 3: Implementa los tipos + `is_under` + `ScopeRegistry` + esqueleto `ScopedPolicy`**

En `policy.rs`:

```rust
//! Policy engine (M3-3, spec §10): cada mutación de un AGENTE se evalúa contra
//! la frontera de su SCOPE (rutas + ops + TTL) y las reglas de `policy.toml`,
//! dando `Allow | Ask | Deny` ANTES de ejecutarse. Los humanos (`User`) no se
//! sandboxean. Enforcement, no prompt-engineering (regla 9).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use norte_proto::{DeleteMode, VPath};

use crate::journal::Actor;

pub use config::{PolicyConfig, Rule, RuleAction};

mod config;

/// Operación evaluable (más detalle que `TaskKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyOp {
    /// Copia.
    Copy,
    /// Movimiento.
    Move,
    /// Borrado (permanente o a papelera).
    Delete {
        /// Permanente vs papelera.
        mode: DeleteMode,
    },
    /// Creación de directorio.
    Mkdir,
}

impl PolicyOp {
    /// Etiqueta estable para reglas/scope (`"copy"|"move"|"delete"|"mkdir"`).
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            PolicyOp::Copy => "copy",
            PolicyOp::Move => "move",
            PolicyOp::Delete { .. } => "delete",
            PolicyOp::Mkdir => "mkdir",
        }
    }
}

/// Conjunto de op-kinds que un scope concede.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpSet {
    kinds: std::collections::BTreeSet<&'static str>,
}

impl OpSet {
    /// Todas las operaciones.
    #[must_use]
    pub fn all() -> Self {
        Self {
            kinds: ["copy", "move", "delete", "mkdir"].into_iter().collect(),
        }
    }
    /// Con un conjunto explícito.
    #[must_use]
    pub fn of(kinds: &[&'static str]) -> Self {
        Self {
            kinds: kinds.iter().copied().collect(),
        }
    }
    /// `true` si `op` está concedida.
    #[must_use]
    pub fn allows(&self, op: PolicyOp) -> bool {
        self.kinds.contains(op.kind())
    }
}

/// Un scope concedido a una sesión de agente: contención por subtree-prefix.
#[derive(Debug, Clone)]
pub struct Scope {
    /// Raíces permitidas (contención por prefijo de segmentos).
    pub roots: Vec<VPath>,
    /// Op-kinds concedidos.
    pub ops: OpSet,
    /// Expiración (TTL). `None` = sin expiración (tests).
    pub expires_at: Option<Instant>,
}

impl Scope {
    /// Scope sin expiración (para tests / grants permanentes).
    #[must_use]
    pub fn forever(roots: Vec<VPath>, ops: OpSet) -> Self {
        Self { roots, ops, expires_at: None }
    }
    fn is_expired(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|e| now >= e)
    }
}

/// `true` si `path` está bajo `root` (mismo scheme+authority y los segmentos de
/// `root` son PREFIJO de los de `path`). Byte-exacto (regla 1): compara
/// segmentos crudos, jamás strings ni prefijo de wire (que confundiría `a`↔`ab`).
#[must_use]
pub fn is_under(root: &VPath, path: &VPath) -> bool {
    if root.scheme() != path.scheme() || root.authority() != path.authority() {
        return false;
    }
    let mut r = root.segments();
    let mut p = path.segments();
    loop {
        match (r.next(), p.next()) {
            (None, _) => return true, // root agotado → path == root o bajo él
            (Some(rs), Some(ps)) if rs == ps => {}
            _ => return false,
        }
    }
}

/// Grants de scope por sesión de agente (en memoria; TTL). Thread-safe.
#[derive(Clone, Default)]
pub struct ScopeRegistry {
    inner: Arc<Mutex<HashMap<String, Vec<Scope>>>>,
}

impl ScopeRegistry {
    /// Registro vacío.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    /// Concede `scope` a la sesión `session`.
    ///
    /// # Panics
    /// Solo si el lock interno queda envenenado.
    pub fn grant(&self, session: &str, scope: Scope) {
        self.inner
            .lock()
            .expect("scope registry lock")
            .entry(session.to_owned())
            .or_default()
            .push(scope);
    }
    /// `true` si algún scope VIVO de `session` contiene `path` para `op`.
    ///
    /// # Panics
    /// Solo si el lock interno queda envenenado.
    #[must_use]
    pub fn permits(&self, session: &str, op: PolicyOp, path: &VPath, now: Instant) -> ScopeVerdict {
        let map = self.inner.lock().expect("scope registry lock");
        let Some(scopes) = map.get(session) else {
            return ScopeVerdict::OutOfScope;
        };
        let mut saw_expired = false;
        for s in scopes {
            if s.is_expired(now) {
                saw_expired = true;
                continue;
            }
            if s.ops.allows(op) && s.roots.iter().any(|r| is_under(r, path)) {
                return ScopeVerdict::Within;
            }
        }
        if saw_expired {
            ScopeVerdict::Expired
        } else {
            ScopeVerdict::OutOfScope
        }
    }
}

/// Resultado de la comprobación de frontera de scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeVerdict {
    /// Dentro de un scope vivo con la op concedida.
    Within,
    /// Fuera de todo scope.
    OutOfScope,
    /// Solo había scopes expirados aplicables.
    Expired,
}

/// Veredicto del motor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Procede sin preguntar.
    Allow,
    /// Requiere aprobación interactiva.
    Ask,
    /// Denegado, con motivo.
    Deny(DenyReason),
}

/// Por qué se denegó.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// Ruta/op fuera del scope del agente.
    OutOfScope,
    /// El scope aplicable expiró.
    ScopeExpired,
    /// Una regla `deny` de `policy.toml`.
    PolicyRule,
    /// Fail-closed: ninguna regla aplicó dentro del scope.
    NoRule,
    /// `Ask` denegado o TTL vencido (lo pone el gate del engine).
    NotApproved,
}

/// El gate que consulta el engine.
pub trait PolicyGate: Send + Sync {
    /// Decide para (actor, op, rutas). Todas las rutas deben pasar la frontera.
    fn evaluate(&self, actor: &Actor, op: PolicyOp, paths: &[&VPath]) -> Decision;
}

/// Gate permisivo (default del engine embebido / sin policy configurada).
pub struct AllowAll;
impl PolicyGate for AllowAll {
    fn evaluate(&self, _actor: &Actor, _op: PolicyOp, _paths: &[&VPath]) -> Decision {
        Decision::Allow
    }
}

/// Policy real: frontera de scope (agentes) + reglas `policy.toml`.
pub struct ScopedPolicy {
    scopes: ScopeRegistry,
    config: PolicyConfig,
}

impl ScopedPolicy {
    /// Con un registro de scopes y una config de reglas.
    #[must_use]
    pub fn new(scopes: ScopeRegistry, config: PolicyConfig) -> Self {
        Self { scopes, config }
    }
    /// Acceso al registro (para conceder scopes en 3b/tests).
    #[must_use]
    pub fn scopes(&self) -> &ScopeRegistry {
        &self.scopes
    }
}
```

Nota: `Instant::now()` no está en la lista de prohibidos (a diferencia de `Date`);
se usa para el TTL. Los tests con `Scope::forever` evitan depender del reloj.

- [ ] **Step 4: Implementa `PolicyGate for ScopedPolicy` (frontera de scope)**

Añade el `impl` (las reglas de `policy.toml` se enganchan en la Task 2; aquí, si
está dentro del scope y no hay reglas, `Deny(NoRule)`):

```rust
impl PolicyGate for ScopedPolicy {
    fn evaluate(&self, actor: &Actor, op: PolicyOp, paths: &[&VPath]) -> Decision {
        let session = match actor {
            Actor::User => return Decision::Allow, // el humano no se sandboxea
            Actor::Agent { session } => session.as_str(),
            Actor::Plugin { id } => id.as_str(),
        };
        let now = Instant::now();
        // Frontera: TODAS las rutas dentro del scope, o deny duro.
        for path in paths {
            match self.scopes.permits(session, op, path, now) {
                ScopeVerdict::Within => {}
                ScopeVerdict::OutOfScope => return Decision::Deny(DenyReason::OutOfScope),
                ScopeVerdict::Expired => return Decision::Deny(DenyReason::ScopeExpired),
            }
        }
        // Dentro del scope: reglas de policy.toml (Task 2). De momento fail-closed.
        self.config.decide(actor, op, paths)
    }
}
```

- [ ] **Step 5: `lib.rs` + reexports**

En `crates/norte-core/src/lib.rs`: `mod policy;` y `pub use policy::{Decision, DenyReason, OpSet, PolicyGate, PolicyOp, Scope, ScopeRegistry, ScopedPolicy};`.

- [ ] **Step 6: Verde**

Run: `cargo nextest run -p norte-core policy::tests`
Expected: PASS (los 4 tests; `PolicyConfig::default().decide` devuelve `Deny(NoRule)` — lo stubeas en Task 2, aquí un stub mínimo que devuelva `Decision::Deny(DenyReason::NoRule)`).

- [ ] **Step 7: Commit**

```bash
git add crates/norte-core/src/policy.rs crates/norte-core/src/lib.rs
git commit -m "feat(core): policy — tipos + scope boundary (subtree-prefix) + ScopeRegistry (M3-3a)"
```

---

## Task 2: `policy.toml` — reglas allow/ask/deny

**Files:**
- Create: `crates/norte-core/src/policy/config.rs`
- Test: `crates/norte-core/src/policy/config.rs` (mod `tests`)

- [ ] **Step 1: Test rojo — parsing + primera-coincidencia**

Crea `config.rs` con tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Actor;
    use norte_proto::{DeleteMode, VPath};

    fn vp(w: &str) -> VPath { VPath::parse(w).expect("wire") }

    #[test]
    fn parses_rules_and_rejects_unknown_fields() {
        let toml = r#"
            [[rule]]
            op = "delete"
            action = "ask"
            [[rule]]
            path_prefix = "file:///tmp"
            action = "allow"
        "#;
        let cfg = PolicyConfig::parse(toml).expect("parse");
        assert_eq!(cfg.rules.len(), 2);
        assert!(PolicyConfig::parse("[[rule]]\nbogus = 1\naction = \"allow\"").is_err());
    }

    #[test]
    fn first_matching_rule_wins() {
        let cfg = PolicyConfig::parse(
            "[[rule]]\nop=\"delete\"\naction=\"deny\"\n[[rule]]\naction=\"allow\"",
        ).expect("parse");
        let agent = Actor::Agent { session: "s".into() };
        // delete → primera regla deny.
        assert_eq!(
            cfg.decide(&agent, PolicyOp::Delete { mode: DeleteMode::Permanent }, &[&vp("file:///x")]),
            Decision::Deny(DenyReason::PolicyRule)
        );
        // copy → segunda regla allow.
        assert_eq!(cfg.decide(&agent, PolicyOp::Copy, &[&vp("file:///x")]), Decision::Allow);
    }

    #[test]
    fn no_rule_matches_is_fail_closed() {
        let cfg = PolicyConfig::parse("[[rule]]\nop=\"mkdir\"\naction=\"allow\"").expect("parse");
        let agent = Actor::Agent { session: "s".into() };
        assert_eq!(
            cfg.decide(&agent, PolicyOp::Copy, &[&vp("file:///x")]),
            Decision::Deny(DenyReason::NoRule)
        );
    }
}
```

- [ ] **Step 2: Verifica que falla**

Run: `cargo nextest run -p norte-core policy::config::tests::first_matching_rule_wins`
Expected: FAIL de compilación.

- [ ] **Step 3: Implementa `PolicyConfig`/`Rule`/`decide`**

```rust
//! Reglas declarativas de `policy.toml` (M3-3): lista ordenada, primera
//! coincidencia gana; sin coincidencia = fail-closed (`Deny(NoRule)`).

use serde::Deserialize;

use norte_proto::VPath;

use crate::journal::Actor;
use crate::policy::{Decision, DenyReason, PolicyOp};

/// Acción de una regla.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    /// Permite sin preguntar.
    Allow,
    /// Requiere aprobación interactiva.
    Ask,
    /// Deniega.
    Deny,
}

/// Una regla; los campos ausentes son comodín.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// `copy|move|delete|mkdir`; ausente = cualquiera.
    #[serde(default)]
    pub op: Option<String>,
    /// Prefijo de `VPath::to_wire`; ausente = cualquiera.
    #[serde(default)]
    pub path_prefix: Option<String>,
    /// Scheme (`file`/`sftp`/…); ausente = cualquiera.
    #[serde(default)]
    pub scheme: Option<String>,
    /// `user|agent|plugin`; ausente = cualquiera.
    #[serde(default)]
    pub actor: Option<String>,
    /// La acción si la regla matchea.
    pub action: RuleAction,
}

impl Rule {
    fn matches(&self, actor_kind: &str, op: PolicyOp, path: &VPath) -> bool {
        self.op.as_deref().is_none_or(|o| o == op.kind())
            && self.actor.as_deref().is_none_or(|a| a == actor_kind)
            && self.scheme.as_deref().is_none_or(|s| s == path.scheme())
            && self
                .path_prefix
                .as_deref()
                .is_none_or(|pre| path.to_wire().starts_with(pre))
    }
}

/// Config de policy: reglas ordenadas.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    /// Reglas en orden; la primera que matcha gana.
    #[serde(default, rename = "rule")]
    pub rules: Vec<Rule>,
}

impl PolicyConfig {
    /// Parsea desde texto TOML.
    ///
    /// # Errors
    /// El error de `toml` si el documento es inválido o tiene campos extra.
    pub fn parse(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Carga desde `config_dir()/policy.toml`; ausente = sin reglas.
    ///
    /// # Errors
    /// Error de lectura (que no sea NotFound) o de parseo.
    pub fn load() -> std::io::Result<Self> {
        let path = crate::connect::config_dir().join("policy.toml");
        match std::fs::read_to_string(&path) {
            Ok(s) => Self::parse(&s)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Decide para TODAS las rutas: la primera regla que matcha CADA path… en
    /// la práctica evaluamos la primera regla que matcha (por path) y tomamos la
    /// más restrictiva. Simplificación M3-3: evalúa contra la primera ruta y
    /// aplica a la op (una op = un scope homogéneo). Sin regla → `Deny(NoRule)`.
    #[must_use]
    pub fn decide(&self, actor: &Actor, op: PolicyOp, paths: &[&VPath]) -> Decision {
        let (actor_kind, _) = actor.parts();
        // Toma la ruta primaria (destino de la op); todas comparten scope.
        let path = paths.first().copied();
        let Some(path) = path else {
            return Decision::Deny(DenyReason::NoRule);
        };
        for rule in &self.rules {
            if rule.matches(actor_kind, op, path) {
                return match rule.action {
                    RuleAction::Allow => Decision::Allow,
                    RuleAction::Ask => Decision::Ask,
                    RuleAction::Deny => Decision::Deny(DenyReason::PolicyRule),
                };
            }
        }
        Decision::Deny(DenyReason::NoRule)
    }
}
```

Ajusta el stub de Task 1 (si dejaste un `decide` mínimo en `policy.rs`, elimínalo:
ahora vive en `config.rs`).

- [ ] **Step 4: Verde**

Run: `cargo nextest run -p norte-core policy::config::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-core/src/policy/config.rs crates/norte-core/src/policy.rs
git commit -m "feat(core): policy.toml reglas allow/ask/deny (primera-gana, fail-closed) (M3-3a)"
```

---

## Task 3: `ApprovalResolver` + wiring en el engine (actor threading + gating)

**Files:**
- Create: `crates/norte-core/src/approval.rs`
- Modify: `crates/norte-core/src/scheduler.rs` (`submit` gana actor)
- Modify: `crates/norte-core/src/engine.rs` (campos, `with_policy`, ops `*_as`, gating, undo)
- Modify: `crates/norte-core/src/lib.rs`

- [ ] **Step 1: `approval.rs`**

```rust
//! Resolución de un `Ask` de policy (M3-3): el engine suspende la llamada hasta
//! que un frontend aprueba/deniega. El default es `DenyAll` (headless
//! fail-closed); el daemon inyecta un resolver que difunde
//! `policy.approval_required` y await-ea `policy.decide` (M3-3b).

use async_trait::async_trait;

use norte_proto::VPath;

use crate::journal::Actor;
use crate::policy::PolicyOp;

/// Descripción de la op a aprobar (preview para el frontend).
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// Quién la pide.
    pub actor: Actor,
    /// Qué operación.
    pub op: PolicyOp,
    /// Rutas implicadas (wire).
    pub paths: Vec<String>,
}

/// Resultado de la aprobación.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// Aprobada por el humano.
    Approved,
    /// Denegada.
    Denied,
    /// TTL vencido sin decisión.
    TimedOut,
}

/// Resuelve un `Ask`. Implementaciones: `DenyAll` (default), y el router del
/// daemon (3b).
#[async_trait]
pub trait ApprovalResolver: Send + Sync {
    /// Pide aprobación y espera el veredicto.
    async fn request(&self, req: ApprovalRequest) -> ApprovalOutcome;
}

/// Deniega todo (headless fail-closed).
pub struct DenyAll;

#[async_trait]
impl ApprovalResolver for DenyAll {
    async fn request(&self, _req: ApprovalRequest) -> ApprovalOutcome {
        ApprovalOutcome::Denied
    }
}
```

En `lib.rs`: `mod approval;` + `pub use approval::{ApprovalOutcome, ApprovalRequest, ApprovalResolver};`.

- [ ] **Step 2: `Scheduler::submit` gana `actor`**

En `scheduler.rs`, cambia la firma y el `TaskCtx`:

```rust
    pub fn submit(
        &self,
        provider_key: &str,
        kind: TaskKind,
        priority: Priority,
        actor: crate::journal::Actor,
        body: TaskBody,
    ) -> TaskHandle {
```
y
```rust
        let ctx = TaskCtx {
            cancel: cancel.clone(),
            progress: Arc::clone(&reporter),
            actor,
        };
```
(quita el comentario/hardcode de `Actor::User`.)

- [ ] **Step 3: Engine — campos + `with_policy` + gating + ops `*_as`**

En `engine.rs`, añade campos:

```rust
    observer: Arc<dyn MutationObserver>,
    journal: Option<Arc<crate::journal::SqliteJournal>>,
    policy: Arc<dyn crate::policy::PolicyGate>,
    approvals: Arc<dyn crate::approval::ApprovalResolver>,
```

Todos los constructores existentes (`with_observer`, `with_journal`) fijan
`policy: Arc::new(crate::policy::AllowAll)` y `approvals: Arc::new(crate::approval::DenyAll)`.
Añade un setter/constructor:

```rust
    /// Instala el gate de policy y el resolver de aprobaciones (M3-3).
    #[must_use]
    pub fn with_policy(
        mut self,
        policy: Arc<dyn crate::policy::PolicyGate>,
        approvals: Arc<dyn crate::approval::ApprovalResolver>,
    ) -> Self {
        self.policy = policy;
        self.approvals = approvals;
        self
    }
```

Helper de gating (privado):

```rust
    /// Evalúa la policy PRE-efecto; `Ask` suspende hasta aprobación. Devuelve
    /// `Err(PermissionDenied)` si se deniega (en 3b será `PolicyDenied`).
    async fn gate(
        &self,
        actor: &crate::journal::Actor,
        op: crate::policy::PolicyOp,
        paths: &[&VPath],
    ) -> Result<(), Error> {
        use crate::policy::Decision;
        match self.policy.evaluate(actor, op, paths) {
            Decision::Allow => Ok(()),
            Decision::Deny(reason) => {
                tracing::info!(?reason, op = op.kind(), "policy denegó la operación");
                Err(Error::PermissionDenied)
            }
            Decision::Ask => {
                let req = crate::approval::ApprovalRequest {
                    actor: actor.clone(),
                    op,
                    paths: paths.iter().map(|p| p.to_wire()).collect(),
                };
                match self.approvals.request(req).await {
                    crate::approval::ApprovalOutcome::Approved => Ok(()),
                    crate::approval::ApprovalOutcome::Denied
                    | crate::approval::ApprovalOutcome::TimedOut => Err(Error::PermissionDenied),
                }
            }
        }
    }
```

Refactoriza `copy_with`/`move_with`/`delete_with` a variantes `*_as(actor)` que
gatean, y deja las actuales como delegadoras `User`. Patrón (para copy):

```rust
    /// Copia con políticas y ACTOR explícito (camino agéntico, M3-3). Gatea por
    /// policy PRE-efecto y registra el actor real en el journal.
    ///
    /// # Errors
    /// [`Error::PermissionDenied`] si la policy deniega; [`Error::Unsupported`]
    /// si algún scheme no tiene provider.
    pub async fn copy_with_as(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
        actor: crate::journal::Actor,
    ) -> Result<TaskHandle, Error> {
        self.gate(&actor, crate::policy::PolicyOp::Copy, &[from, to]).await?;
        let src = self.provider_for(from).await?;
        let dst = self.provider_for(to).await?;
        let observer = Arc::clone(&self.observer);
        let (from, to) = (from.clone(), to.clone());
        let key = to.scheme().to_owned();
        Ok(self.sched.submit(
            &key,
            TaskKind::Copy,
            Priority::Normal,
            actor,
            Box::new(move |ctx| {
                Box::pin(async move { ops::copy_task(src, dst, from, to, opts, observer, &ctx).await })
            }),
        ))
    }

    /// Copia con políticas (actor `User`).
    pub async fn copy_with(&self, from: &VPath, to: &VPath, opts: TransferOptions) -> Result<TaskHandle, Error> {
        self.copy_with_as(from, to, opts, crate::journal::Actor::User).await
    }
```

Haz lo análogo para `move_with_as` (`PolicyOp::Move`, paths `&[from, to]`) y
`delete_with_as` (`PolicyOp::Delete { mode }`, paths `&[path]`). Todas las demás
llamadas a `self.sched.submit(...)` de `engine.rs` reciben ahora el `actor`
(pásalo desde la variante `*_as`).

- [ ] **Step 4: `undo_session` gatea por entrada (cierra deuda M3 de M3-2)**

Antes de ejecutar cada reversa, gatea. Como el gating es async y el cuerpo de la
Task ya es async, evalúa DENTRO del bucle (mapea la entrada a un `PolicyOp`
inverso):

En `undo_session`, tras resolver el provider y ANTES de spawnear, guarda el
`policy`/`approvals` en el plan, o evalúa el gate en la fase de planning (más
simple y consistente con "para en el primer bloqueo"): para cada entrada, mapea
su `reversal` a un `PolicyOp` (delete→`Mkdir`? no: la reversa de `created` es un
borrado → `PolicyOp::Delete{Permanent}`; `rename_back`→`Move`; `restore_trash`→
`Copy`/`Move`) y llama `self.gate(&actor, op, &paths).await`; si deniega, marca
`blocked` y para.

Decisión de plan: gatea en el bucle de la Task NO es posible (`&self` no viaja).
Por eso: en la fase de PLANNING de `undo_session` (que ya tiene `&self`), evalúa
el gate por entrada y si alguna deniega, corta el plan ahí (report `blocked` con
`PermissionDenied`). Añade tras resolver el provider:

```rust
            // Gate de policy (M3-3): el undo pasa por la misma policy. La reversa
            // de un Created BORRA → PolicyOp::Delete; rename_back → Move;
            // restore_trash → Move.
            let undo_op = match e.reversal.as_str() {
                "delete" => crate::policy::PolicyOp::Delete { mode: norte_proto::DeleteMode::Permanent },
                "rename_back" | "restore_trash" => crate::policy::PolicyOp::Move,
                _ => crate::policy::PolicyOp::Delete { mode: norte_proto::DeleteMode::Permanent },
            };
            if let Err(err) = self.gate(&actor, undo_op, &[&p]).await {
                report.lock().expect("undo report lock").blocked = Some((e.seq, err));
                break; // estricto: para al primer bloqueo de policy.
            }
```
(Este `break` corta el bucle de PLANNING; el resto del plan no se añade. Ajusta:
recolecta el plan en un `Vec` y sal del `for` con `break` conservando lo ya
planificado. Como el planning es LIFO, lo ya planificado es lo más reciente.)

- [ ] **Step 5: Migra los call-sites de `submit`**

Cualquier otro `self.sched.submit(...)` en `engine.rs` (p. ej. el de
`undo_session`) recibe el `actor` correspondiente (`actor.clone()` en undo).

- [ ] **Step 6: Compila + tests existentes verdes**

Run: `cargo build -p norte-core`
Expected: OK.
Run: `cargo nextest run -p norte-core`
Expected: PASS (los tests que usan `copy`/`move_`/`delete`/`copy_with` siguen
como `User`→Allow; `undo` sigue verde con `AllowAll` default).

- [ ] **Step 7: Commit**

```bash
git add crates/norte-core/src/approval.rs crates/norte-core/src/scheduler.rs crates/norte-core/src/engine.rs crates/norte-core/src/lib.rs
git commit -m "feat(core): gate de policy PRE-efecto + actor threading + undo bajo policy (M3-3a)"
```

---

## Task 4: tests de integración del gating

**Files:**
- Create: `crates/norte-core/tests/policy.rs`

- [ ] **Step 1: Escenarios engine↔policy**

Crea `crates/norte-core/tests/policy.rs`:

```rust
//! Integración policy↔engine (M3-3a): el gate PRE-efecto decide allow/ask/deny
//! por operación; el actor real llega al journal; el undo pasa por la policy.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use norte_core::approval::{ApprovalOutcome, ApprovalRequest, ApprovalResolver};
use norte_core::journal::Actor;
use norte_core::policy::{OpSet, PolicyConfig, Scope, ScopeRegistry, ScopedPolicy};
use norte_core::{Engine, Journal, SqliteJournal};
use norte_proto::{Error, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::{ByteSink, Provider};

fn vp(w: &str) -> VPath { VPath::parse(w).expect("wire") }

async fn write_file(mem: &MemProvider, w: &str, c: &[u8]) {
    let mut s = mem.write(&vp(w)).await.expect("open");
    s.write(Bytes::copy_from_slice(c)).await.expect("chunk");
    s.commit().await.expect("commit");
}

struct AlwaysApprove;
#[async_trait]
impl ApprovalResolver for AlwaysApprove {
    async fn request(&self, _r: ApprovalRequest) -> ApprovalOutcome { ApprovalOutcome::Approved }
}

/// Engine con journal + policy `ScopedPolicy` y un resolver dado.
async fn engine_with_policy(
    config: PolicyConfig,
    scopes: ScopeRegistry,
    resolver: Arc<dyn ApprovalResolver>,
) -> (Engine, Arc<MemProvider>, Arc<SqliteJournal>) {
    let journal = Arc::new(SqliteJournal::new(Journal::open_in_memory().await.expect("j")));
    let policy = Arc::new(ScopedPolicy::new(scopes, config));
    let engine = Engine::with_journal(Arc::clone(&journal)).with_policy(policy, resolver);
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem, journal)
}

#[tokio::test]
async fn agent_out_of_scope_copy_is_denied_without_touching_fs() {
    let (engine, mem, _j) = engine_with_policy(
        PolicyConfig::default(), ScopeRegistry::new(), Arc::new(norte_core::approval::DenyAll),
    ).await;
    write_file(&mem, "mem:///src.txt", b"x").await;
    let agent = Actor::Agent { session: "s1".into() };
    let err = engine
        .copy_with_as(&vp("mem:///src.txt"), &vp("mem:///dst.txt"), Default::default(), agent)
        .await
        .err()
        .expect("denegada");
    assert!(matches!(err, Error::PermissionDenied));
    assert!(matches!(mem.stat(&vp("mem:///dst.txt")).await, Err(Error::NotFound)), "no tocó el FS");
}

#[tokio::test]
async fn agent_in_scope_with_allow_rule_proceeds_and_journals_actor() {
    let scopes = ScopeRegistry::new();
    scopes.grant("s1", Scope::forever(vec![vp("mem:///")], OpSet::all()));
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"allow\"").expect("cfg");
    let (engine, mem, journal) = engine_with_policy(cfg, scopes, Arc::new(norte_core::approval::DenyAll)).await;
    write_file(&mem, "mem:///src.txt", b"x").await;
    let agent = Actor::Agent { session: "s1".into() };
    let h = engine
        .copy_with_as(&vp("mem:///src.txt"), &vp("mem:///dst.txt"), Default::default(), agent)
        .await
        .expect("submit");
    assert_eq!(h.join().await, TaskState::Completed);
    // El journal registró la creación con actor AGENT (cierra deuda M2 de M3-2).
    let es = journal.journal().entries().await.expect("entries");
    let created = es.iter().find(|e| e.op == "created").expect("created");
    assert_eq!(created.actor_kind, "agent");
    assert_eq!(created.actor_id.as_deref(), Some("s1"));
}

#[tokio::test]
async fn ask_rule_suspends_until_resolver_approves() {
    let scopes = ScopeRegistry::new();
    scopes.grant("s1", Scope::forever(vec![vp("mem:///")], OpSet::all()));
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"ask\"").expect("cfg");
    let (engine, mem, _j) = engine_with_policy(cfg, scopes, Arc::new(AlwaysApprove)).await;
    write_file(&mem, "mem:///src.txt", b"x").await;
    let agent = Actor::Agent { session: "s1".into() };
    let h = engine
        .copy_with_as(&vp("mem:///src.txt"), &vp("mem:///dst.txt"), Default::default(), agent)
        .await
        .expect("aprobada procede");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(mem.stat(&vp("mem:///dst.txt")).await.is_ok());
}

#[tokio::test]
async fn ask_denied_by_resolver_blocks_the_op() {
    let scopes = ScopeRegistry::new();
    scopes.grant("s1", Scope::forever(vec![vp("mem:///")], OpSet::all()));
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"ask\"").expect("cfg");
    // DenyAll resolver → Ask se resuelve Denied.
    let (engine, mem, _j) = engine_with_policy(cfg, scopes, Arc::new(norte_core::approval::DenyAll)).await;
    write_file(&mem, "mem:///src.txt", b"x").await;
    let agent = Actor::Agent { session: "s1".into() };
    let err = engine
        .copy_with_as(&vp("mem:///src.txt"), &vp("mem:///dst.txt"), Default::default(), agent)
        .await
        .err()
        .expect("denegada");
    assert!(matches!(err, Error::PermissionDenied));
    assert!(matches!(mem.stat(&vp("mem:///dst.txt")).await, Err(Error::NotFound)));
}

#[tokio::test]
async fn user_bypasses_policy() {
    // Sin scope ni reglas, un User copia igual (no se sandboxea).
    let (engine, mem, _j) = engine_with_policy(
        PolicyConfig::default(), ScopeRegistry::new(), Arc::new(norte_core::approval::DenyAll),
    ).await;
    write_file(&mem, "mem:///src.txt", b"x").await;
    let h = engine.copy(&vp("mem:///src.txt"), &vp("mem:///dst.txt")).await.expect("submit");
    assert_eq!(h.join().await, TaskState::Completed);
}

#[tokio::test]
async fn undo_of_agent_out_of_scope_is_blocked_by_policy() {
    // Un agente crea dentro de scope; luego el scope se retira; el undo del
    // agente ya no pasa la policy.
    let scopes = ScopeRegistry::new();
    scopes.grant("s1", Scope::forever(vec![vp("mem:///")], OpSet::all()));
    let cfg = PolicyConfig::parse("[[rule]]\naction=\"allow\"").expect("cfg");
    let (engine, mem, _j) = engine_with_policy(cfg, scopes.clone(), Arc::new(norte_core::approval::DenyAll)).await;
    write_file(&mem, "mem:///src.txt", b"x").await;
    let agent = Actor::Agent { session: "s1".into() };
    let h = engine
        .copy_with_as(&vp("mem:///src.txt"), &vp("mem:///dst.txt"), Default::default(), agent.clone())
        .await
        .expect("submit");
    assert_eq!(h.join().await, TaskState::Completed);
    // El undo de un agente cuya op de reversa (delete) ninguna regla permite
    // (solo hay allow para... de hecho la regla es allow-todo, así que para
    // probar el bloqueo, cambia la config): ver nota.
    let _ = (engine, agent);
}
```

Nota del último test: con `action="allow"` universal el undo también se permite;
para probar el BLOQUEO del undo por policy, usa una config que permita `copy`
pero deny `delete` (la reversa de un Created es un delete):
`[[rule]]\nop="copy"\naction="allow"\n[[rule]]\nop="delete"\naction="deny"`.
Reescribe el test para: crear con agente (copy allow) → `undo_session(agent)` →
`report.blocked` es `Some((_, PermissionDenied))` y el nodo sigue existiendo.

- [ ] **Step 2: Verde**

Run: `cargo nextest run -p norte-core --test policy`
Expected: PASS.

- [ ] **Step 3: security-reviewer + encoding-auditor + rust-reviewer + commit**

`security-reviewer` (OBLIGATORIO): fail-closed real, frontera de scope sin
bypass, `is_under` sin escape, el gate cubre TODAS las mutaciones (copy/move/
delete/trash/undo), `User`-bypass es intencional y no explotable por un agente
que se declare `User` (el actor lo fija el core, no el agente — confírmalo para
3b). `encoding-auditor` (`is_under`/path_prefix sobre bytes). `rust-reviewer`.

```bash
git add crates/norte-core/tests/policy.rs
git commit -m "test(core): integración policy↔engine (scope/ask/deny/undo, actor→journal) (M3-3a)"
```

---

## Task 5: cierre

- [ ] **Step 1: rustdoc + sin TODO sueltos**

`#![warn(missing_docs)]`: documenta todo item público de `policy`/`approval`.

- [ ] **Step 2: `just ci` verde**

Run: `just ci`
Expected: fmt + clippy `-D warnings` + nextest + deny + cobertura ≥85% + doc. El
módulo `policy` es lógica de decisión → apunta a cobertura alta (spec §242).

- [ ] **Step 3: Memoria**

`proyecto-norte-estado.md` + `MEMORY.md`: **M3-3a COMPLETA** (policy core: scope
subtree-prefix + policy.toml + gate PRE-efecto + actor threading + undo bajo
policy; cierra deudas actor/undo-policy de M3-2; sin wire — `PermissionDenied`).
**SIGUIENTE: M3-3b** (proto `policy.*` + daemon approval router + TUI modal +
`Error::PolicyDenied` bump 0.11.0).

- [ ] **Step 4: Commit cierre**

```bash
git add docs
git commit -m "docs: cierre M3-3a policy core (memoria)"
```

---

## Self-Review

- **Cobertura del spec (3a):** tipos+eval (T1) ✅; policy.toml (T2) ✅;
  ApprovalResolver+DenyAll (T3) ✅; actor threading en submit/engine (T3, cierra
  deuda M2) ✅; gating PRE-efecto copy/move/delete (T3) ✅; undo bajo policy (T3,
  cierra deuda M3) ✅; AllowAll default sin policy (T3) ✅; tests (T4) ✅.
- **Sin wire:** denegación = `Error::PermissionDenied` (existente); `PolicyDenied`
  y todo `policy.*` = 3b. Sin protocol-guardian aquí.
- **Consistencia de tipos:** `PolicyOp`/`Decision`/`DenyReason`/`Scope`/`OpSet`/
  `ScopeRegistry`/`ScopedPolicy`/`PolicyGate`/`AllowAll` (policy.rs) y
  `PolicyConfig`/`Rule`/`RuleAction` (config.rs) usados igual en engine y tests.
  `ApprovalResolver`/`ApprovalOutcome`/`ApprovalRequest`/`DenyAll` (approval.rs).
  `submit(..., actor, body)` uniforme. `copy_with_as`/`move_with_as`/
  `delete_with_as` + delegadores `User`.

## Riesgos / verificar en ejecución

1. `is_none_or` (Rust 1.82+) — la toolchain está en 1.96.1, disponible. Si no,
   usar `map_or(true, …)`.
2. `submit` gana un parámetro → TODOS los call-sites en `engine.rs` (copy/move/
   delete/undo) deben pasar `actor`. Grep `self.sched.submit` para no dejar
   ninguno.
3. El gate del undo corta el bucle de PLANNING (no el de la Task) — asegúrate de
   conservar el plan ya acumulado y salir con `break`.
4. `DeleteMode` viene de `norte_proto` — impórtalo en `policy.rs`.
5. `Default` para `TransferOptions` en los tests (`Default::default()`).
6. Cobertura: `policy` es lógica pura → añade tests de `OpSet`/`Scope` expirado si
   la cobertura de `policy.rs` baja del gate.
