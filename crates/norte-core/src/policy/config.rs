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
#[derive(Debug, Clone, Deserialize)]
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
    /// Error de lectura (que no sea `NotFound`) o de parseo.
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

    /// Decide para la op sobre `paths` (todas comparten scope; se evalúa contra
    /// la ruta primaria). Primera regla que matcha gana; sin regla →
    /// `Deny(NoRule)` (fail-closed).
    #[must_use]
    pub fn decide(&self, actor: &Actor, op: PolicyOp, paths: &[&VPath]) -> Decision {
        let (actor_kind, _) = actor.parts();
        let Some(path) = paths.first().copied() else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Actor;
    use norte_proto::{DeleteMode, VPath};

    fn vp(w: &str) -> VPath {
        VPath::parse(w).expect("wire")
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
    fn first_matching_rule_wins() {
        let cfg = PolicyConfig::parse(
            "[[rule]]\nop=\"delete\"\naction=\"deny\"\n[[rule]]\naction=\"allow\"",
        )
        .expect("parse");
        let agent = Actor::Agent {
            session: "s".into(),
        };
        assert_eq!(
            cfg.decide(
                &agent,
                PolicyOp::Delete {
                    mode: DeleteMode::Permanent
                },
                &[&vp("file:///x")]
            ),
            Decision::Deny(DenyReason::PolicyRule)
        );
        assert_eq!(
            cfg.decide(&agent, PolicyOp::Copy, &[&vp("file:///x")]),
            Decision::Allow
        );
    }

    #[test]
    fn no_rule_matches_is_fail_closed() {
        let cfg = PolicyConfig::parse("[[rule]]\nop=\"mkdir\"\naction=\"allow\"").expect("parse");
        let agent = Actor::Agent {
            session: "s".into(),
        };
        assert_eq!(
            cfg.decide(&agent, PolicyOp::Copy, &[&vp("file:///x")]),
            Decision::Deny(DenyReason::NoRule)
        );
    }

    #[test]
    fn path_prefix_and_scheme_conditions() {
        let cfg = PolicyConfig::parse(
            "[[rule]]\nscheme=\"sftp\"\naction=\"deny\"\n[[rule]]\npath_prefix=\"file:///tmp\"\naction=\"allow\"\n[[rule]]\naction=\"ask\"",
        )
        .expect("parse");
        let agent = Actor::Agent {
            session: "s".into(),
        };
        assert_eq!(
            cfg.decide(&agent, PolicyOp::Copy, &[&vp("sftp://h/x")]),
            Decision::Deny(DenyReason::PolicyRule)
        );
        assert_eq!(
            cfg.decide(&agent, PolicyOp::Copy, &[&vp("file:///tmp/x")]),
            Decision::Allow
        );
        assert_eq!(
            cfg.decide(&agent, PolicyOp::Copy, &[&vp("file:///home/x")]),
            Decision::Ask
        );
    }
}
