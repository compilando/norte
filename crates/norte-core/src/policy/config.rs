//! Reglas declarativas de `policy.toml` (M3-3): lista ordenada, primera
//! coincidencia gana; sin coincidencia = fail-closed (`Deny(NoRule)`). Para una
//! op de varias rutas (copy/move) se evalúa CADA ruta y gana la MÁS restrictiva
//! (`Deny > Ask > Allow`) — así una regla que protege el destino no se salta.

use serde::Deserialize;

use norte_proto::VPath;

use crate::journal::Actor;
use crate::policy::{Decision, DenyReason, PolicyOp, is_under};

/// Error al cargar/parsear `policy.toml`.
#[derive(Debug, thiserror::Error)]
pub enum PolicyConfigError {
    /// TOML inválido o con campos desconocidos.
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
    /// Un `path_prefix` que no es un `VPath` válido (se valida al cargar para
    /// no fallar en silencio en tiempo de evaluación).
    #[error("path_prefix inválido «{0}»: no es un VPath")]
    BadPrefix(String),
}

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
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// `copy|move|delete|mkdir`; ausente = cualquiera.
    #[serde(default)]
    pub op: Option<String>,
    /// Prefijo por SUBTREE (contención de segmentos, byte-exacta vía
    /// [`is_under`]): debe ser un `VPath` válido (p. ej. `"file:///home/agent"`)
    /// y matchea ese nodo y su subárbol — NUNCA un hermano `…/agent-evil`
    /// (a diferencia de un prefijo de string). Ausente = cualquiera.
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
            && self.path_prefix.as_deref().is_none_or(|pre| {
                // Contención por SEGMENTOS (byte-exacta), no prefijo de string:
                // evita el bug `a`↔`ab` y los `%XX` partidos del wire. Un prefix
                // no parseable (validado al cargar) no matchea, conservador.
                VPath::parse(pre).is_ok_and(|root| is_under(&root, path))
            })
    }
}

/// Config de policy: reglas ordenadas.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    /// Reglas en orden; la primera que matcha gana (por ruta).
    #[serde(default, rename = "rule")]
    pub rules: Vec<Rule>,
}

/// La MÁS restrictiva de dos decisiones (`Deny > Ask > Allow`).
fn more_restrictive(a: Decision, b: Decision) -> Decision {
    fn rank(d: &Decision) -> u8 {
        match d {
            Decision::Deny(_) => 2,
            Decision::Ask => 1,
            Decision::Allow => 0,
        }
    }
    if rank(&b) > rank(&a) { b } else { a }
}

impl PolicyConfig {
    /// Parsea desde texto TOML y valida que todo `path_prefix` sea un `VPath`.
    ///
    /// # Errors
    /// [`PolicyConfigError::Toml`] si el documento es inválido o con campos
    /// extra; [`PolicyConfigError::BadPrefix`] si un `path_prefix` no parsea.
    pub fn parse(s: &str) -> Result<Self, PolicyConfigError> {
        let cfg: Self = toml::from_str(s)?;
        for r in &cfg.rules {
            if let Some(pre) = &r.path_prefix
                && VPath::parse(pre).is_err()
            {
                return Err(PolicyConfigError::BadPrefix(pre.clone()));
            }
        }
        Ok(cfg)
    }

    /// Carga desde `config_dir()/policy.toml`; ausente = sin reglas. SÍNCRONA
    /// (arranque): no invocar desde contexto async sin `spawn_blocking`.
    ///
    /// # Errors
    /// Error de lectura (que no sea `NotFound`) o de parseo/validación.
    pub fn load() -> std::io::Result<Self> {
        let path = crate::connect::config_dir().join("policy.toml");
        match std::fs::read_to_string(&path) {
            Ok(s) => {
                Self::parse(&s).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Decide para la op sobre TODAS las rutas: por cada ruta, la primera regla
    /// que matcha; el resultado es la MÁS restrictiva de todas. Sin regla para
    /// alguna ruta → `Deny(NoRule)` (fail-closed). `paths` vacío → `Deny(NoRule)`.
    #[must_use]
    pub fn decide(&self, actor: &Actor, op: PolicyOp, paths: &[&VPath]) -> Decision {
        if paths.is_empty() {
            return Decision::Deny(DenyReason::NoRule);
        }
        let (actor_kind, _) = actor.parts();
        let mut acc = Decision::Allow;
        for path in paths {
            acc = more_restrictive(acc, self.decide_one(actor_kind, op, path));
        }
        acc
    }

    fn decide_one(&self, actor_kind: &str, op: PolicyOp, path: &VPath) -> Decision {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Actor;
    use norte_proto::{DeleteMode, VPath};

    fn vp(w: &str) -> VPath {
        VPath::parse(w).expect("wire")
    }

    fn agent() -> Actor {
        Actor::Agent {
            session: "s".into(),
        }
    }

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
    fn rejects_unparseable_path_prefix() {
        // Un prefix que no es VPath válido falla al cargar (no en silencio).
        assert!(matches!(
            PolicyConfig::parse("[[rule]]\npath_prefix=\"no-un-vpath\"\naction=\"allow\""),
            Err(PolicyConfigError::BadPrefix(_))
        ));
    }

    #[test]
    fn first_matching_rule_wins() {
        let cfg = PolicyConfig::parse(
            "[[rule]]\nop=\"delete\"\naction=\"deny\"\n[[rule]]\naction=\"allow\"",
        )
        .expect("parse");
        assert_eq!(
            cfg.decide(
                &agent(),
                PolicyOp::Delete {
                    mode: DeleteMode::Permanent
                },
                &[&vp("file:///x")]
            ),
            Decision::Deny(DenyReason::PolicyRule)
        );
        assert_eq!(
            cfg.decide(&agent(), PolicyOp::Copy, &[&vp("file:///x")]),
            Decision::Allow
        );
    }

    #[test]
    fn no_rule_matches_is_fail_closed() {
        let cfg = PolicyConfig::parse("[[rule]]\nop=\"mkdir\"\naction=\"allow\"").expect("parse");
        assert_eq!(
            cfg.decide(&agent(), PolicyOp::Copy, &[&vp("file:///x")]),
            Decision::Deny(DenyReason::NoRule)
        );
    }

    #[test]
    fn path_prefix_is_segment_boundary_not_string_prefix() {
        // ALTA (encoding-auditor A): `file:///home/agent` NO cubre un hermano
        // `…/agent-evil`, aunque el string lo prefije.
        let cfg = PolicyConfig::parse(
            "[[rule]]\npath_prefix=\"file:///home/agent\"\naction=\"allow\"\n[[rule]]\naction=\"deny\"",
        )
        .expect("parse");
        assert_eq!(
            cfg.decide(&agent(), PolicyOp::Copy, &[&vp("file:///home/agent/x")]),
            Decision::Allow
        );
        assert_eq!(
            cfg.decide(
                &agent(),
                PolicyOp::Copy,
                &[&vp("file:///home/agent-evil/x")]
            ),
            Decision::Deny(DenyReason::PolicyRule),
            "un hermano NO cae en el allow del prefijo"
        );
    }

    #[test]
    fn multi_path_op_takes_most_restrictive() {
        // MAJOR (security M1): una regla que protege el DESTINO no se salta por
        // evaluar solo el origen.
        let cfg = PolicyConfig::parse(
            "[[rule]]\npath_prefix=\"file:///work/.ssh\"\naction=\"deny\"\n[[rule]]\naction=\"allow\"",
        )
        .expect("parse");
        // copy from work/data (allow) → work/.ssh/keys (deny) ⇒ Deny.
        let d = cfg.decide(
            &agent(),
            PolicyOp::Copy,
            &[&vp("file:///work/data"), &vp("file:///work/.ssh/keys")],
        );
        assert_eq!(d, Decision::Deny(DenyReason::PolicyRule));
    }
}
