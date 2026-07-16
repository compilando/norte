//! Policy engine (M3-3, spec §10): cada mutación de un AGENTE se evalúa contra
//! la frontera de su SCOPE (rutas + ops + TTL) y las reglas de `policy.toml`,
//! dando `Allow | Ask | Deny` ANTES de ejecutarse. Los humanos (`User`) no se
//! sandboxean. Enforcement, no prompt-engineering (regla 9).
//!
//! **Requisitos de seguridad para M3-3b/M3-4 (deuda anotada):**
//! - El daemon DEBE instalar `ScopedPolicy` vía [`crate::Engine::with_policy`];
//!   el default [`AllowAll`] es solo para el engine embebido/humano. Con agentes,
//!   olvidar el wiring = sin sandbox (fail-open estructural, security M3).
//! - El actor lo fija el CORE por conexión autenticada, jamás se lee de params
//!   del wire: un cliente agéntico no debe poder declararse `User` (que
//!   cortocircuita a `Allow`).
//! - [`ScopeRegistry`] hoy clava por id string; namespacear por `(actor_kind,
//!   id)` para que un Plugin no herede el scope de un Agent homónimo (m4) y
//!   añadir `revoke(session)` para respuesta a incidentes (m7). Relacionado
//!   (#66): la sesión la RECLAMA el cliente en su `initialize` — un agente
//!   hostil-cooperante puede declarar la sesión de OTRO agente y heredar sus
//!   scopes, ver/cancelar sus tasks y recibir su `task.progress`. Aceptado
//!   bajo el threat model same-uid (§14, guardarraíl no sandbox); si algún
//!   día hace falta aislar agente-de-agente, la sesión necesita prueba de
//!   posesión (token al crearla), no solo nombre.
//! - El resolver de `Ask` (M3-3b) DEBE aplicar TTL/timeout y ser cancelable: el
//!   gate suspende la llamada del engine fuera del framework de Task (m5).
//! - Escape de scope vía symlink dentro→fuera que el provider siga (m6): la
//!   mitigación es la política de symlinks del provider (ADR 0005) + fixture
//!   hostil; el gate razona sobre `VPath`s lógicos.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use norte_proto::{DeleteMode, VPath};

use crate::journal::Actor;

pub use config::{PolicyConfig, PolicyConfigError, Rule, RuleAction};

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
    kinds: BTreeSet<&'static str>,
}

impl OpSet {
    /// Todas las operaciones.
    #[must_use]
    pub fn all() -> Self {
        Self {
            kinds: ["copy", "move", "delete", "mkdir"].into_iter().collect(),
        }
    }
    /// Con un conjunto explícito de op-kinds.
    #[must_use]
    pub fn of(kinds: &[&'static str]) -> Self {
        Self {
            kinds: kinds.iter().copied().collect(),
        }
    }
    /// Desde nombres en RUNTIME (wire): conserva cada nombre que sea un op-kind
    /// canónico y DESCARTA los desconocidos (fail-closed — un op-kind que no
    /// reconocemos no se concede jamás). La fuente de verdad de los kinds
    /// válidos es [`Self::all`]: añadir una op ahí la hace concedible por wire
    /// sin tocar este método. Usado por el daemon al conceder un scope pedido.
    #[must_use]
    pub fn from_names<S: AsRef<str>>(names: &[S]) -> Self {
        let all = Self::all();
        let kinds = names
            .iter()
            .filter_map(|n| all.kinds.iter().copied().find(|k| *k == n.as_ref()))
            .collect();
        Self { kinds }
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
    /// Expiración (TTL). `None` = sin expiración (tests / grants permanentes).
    pub expires_at: Option<Instant>,
}

impl Scope {
    /// Scope sin expiración (para tests / grants permanentes).
    #[must_use]
    pub fn forever(roots: Vec<VPath>, ops: OpSet) -> Self {
        Self {
            roots,
            ops,
            expires_at: None,
        }
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
        let now = Instant::now();
        let mut map = self.inner.lock().expect("scope registry lock");
        let entry = map.entry(session.to_owned()).or_default();
        // Poda perezosa: al conceder, descarta los scopes ya expirados de esta
        // sesión para que el `Vec` no crezca monótono en un daemon longevo. La
        // recolección de claves de sesiones muertas es deuda m7 (revoke).
        entry.retain(|s| !s.is_expired(now));
        entry.push(scope);
    }
    /// Veredicto de frontera para `(session, op, path)` a instante `now`.
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

impl DenyReason {
    /// Identificador estable de la causa, tal como viaja en
    /// [`norte_proto::Error::PolicyDenied`]`.rule` por el wire (M3-3b). El
    /// cliente lo compara por igualdad, jamás parsea texto libre. El conjunto
    /// es CERRADO y contractual: su enumeración autoritativa vive en el rustdoc
    /// de `PolicyDenied.rule`; cambiar un string aquí es cambio de wire.
    #[must_use]
    pub fn rule_id(self) -> &'static str {
        match self {
            DenyReason::OutOfScope => "out-of-scope",
            DenyReason::ScopeExpired => "scope-expired",
            DenyReason::PolicyRule => "policy-rule",
            DenyReason::NoRule => "no-rule",
            DenyReason::NotApproved => "not-approved",
        }
    }
}

/// El gate que consulta el engine antes de cada mutación.
pub trait PolicyGate: Send + Sync {
    /// Decide para (actor, op, rutas). TODAS las rutas deben pasar la frontera.
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
        // Dentro del scope: reglas de policy.toml (fail-closed sin regla).
        self.config.decide(actor, op, paths)
    }
}

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
        let pol = ScopedPolicy::new(ScopeRegistry::new(), PolicyConfig::default());
        let d = pol.evaluate(
            &Actor::User,
            PolicyOp::Delete {
                mode: DeleteMode::Permanent,
            },
            &[&vp("file:///x")],
        );
        assert!(matches!(d, Decision::Allow));
    }

    #[test]
    fn deny_reason_rule_ids_are_the_closed_wire_vocabulary() {
        // Pin del conjunto CERRADO que viaja en PolicyDenied.rule: renombrar
        // cualquiera es cambio de wire y debe romper aquí (no en silencio).
        assert_eq!(DenyReason::OutOfScope.rule_id(), "out-of-scope");
        assert_eq!(DenyReason::ScopeExpired.rule_id(), "scope-expired");
        assert_eq!(DenyReason::PolicyRule.rule_id(), "policy-rule");
        assert_eq!(DenyReason::NoRule.rule_id(), "no-rule");
        assert_eq!(DenyReason::NotApproved.rule_id(), "not-approved");
    }

    #[test]
    fn agent_out_of_scope_is_denied() {
        let pol = ScopedPolicy::new(ScopeRegistry::new(), PolicyConfig::default());
        let agent = Actor::Agent {
            session: "s1".into(),
        };
        let d = pol.evaluate(&agent, PolicyOp::Copy, &[&vp("file:///a/x")]);
        assert!(matches!(d, Decision::Deny(DenyReason::OutOfScope)));
    }

    #[test]
    fn agent_in_scope_without_rule_is_denied_fail_closed() {
        let reg = ScopeRegistry::new();
        reg.grant("s1", Scope::forever(vec![vp("file:///a")], OpSet::all()));
        let pol = ScopedPolicy::new(reg, PolicyConfig::default());
        let agent = Actor::Agent {
            session: "s1".into(),
        };
        let d = pol.evaluate(&agent, PolicyOp::Copy, &[&vp("file:///a/x")]);
        assert!(
            matches!(d, Decision::Deny(DenyReason::NoRule)),
            "fail-closed sin regla"
        );
    }

    #[test]
    fn scope_containment_is_byte_exact_prefix() {
        assert!(is_under(&vp("file:///a"), &vp("file:///a/x")));
        assert!(is_under(&vp("file:///a"), &vp("file:///a")));
        assert!(!is_under(&vp("file:///a"), &vp("file:///ab")));
        assert!(!is_under(&vp("file:///a"), &vp("file:///b/x")));
        assert!(!is_under(&vp("file:///a"), &vp("mem:///a/x")));
    }

    #[test]
    fn op_not_granted_is_out_of_scope() {
        let reg = ScopeRegistry::new();
        // Solo copy concedido; un delete queda fuera de scope.
        reg.grant(
            "s1",
            Scope::forever(vec![vp("file:///a")], OpSet::of(&["copy"])),
        );
        let pol = ScopedPolicy::new(reg, PolicyConfig::default());
        let agent = Actor::Agent {
            session: "s1".into(),
        };
        let d = pol.evaluate(
            &agent,
            PolicyOp::Delete {
                mode: DeleteMode::Permanent,
            },
            &[&vp("file:///a/x")],
        );
        assert!(matches!(d, Decision::Deny(DenyReason::OutOfScope)));
    }

    #[test]
    fn expired_scope_denies_with_scope_expired() {
        let reg = ScopeRegistry::new();
        reg.grant(
            "s1",
            Scope {
                roots: vec![vp("file:///a")],
                ops: OpSet::all(),
                // Expiró hace rato.
                expires_at: Some(
                    Instant::now()
                        .checked_sub(std::time::Duration::from_secs(1))
                        .expect("instante en el pasado"),
                ),
            },
        );
        let pol = ScopedPolicy::new(reg, PolicyConfig::default());
        let agent = Actor::Agent {
            session: "s1".into(),
        };
        let d = pol.evaluate(&agent, PolicyOp::Copy, &[&vp("file:///a/x")]);
        assert!(matches!(d, Decision::Deny(DenyReason::ScopeExpired)));
    }
}
