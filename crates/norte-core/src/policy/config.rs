//! Declarative `policy.toml` rules (M3-3): an ordered list, first match wins;
//! no match = fail-closed (`Deny(NoRule)`). For a multi-path op (copy/move)
//! EACH path is evaluated and the MOST restrictive wins (`Deny > Ask >
//! Allow`) — so a rule protecting the destination is never skipped.

use serde::Deserialize;

use norte_proto::VPath;

use crate::journal::Actor;
use crate::policy::{Decision, DenyReason, PolicyOp, is_under};

/// Error loading/parsing `policy.toml`.
#[derive(Debug, thiserror::Error)]
pub enum PolicyConfigError {
    /// Invalid TOML or with unknown fields.
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
    /// A `path_prefix` that is not a valid `VPath` (validated at load time
    /// so it doesn't fail silently at evaluation time).
    #[error("invalid path_prefix «{0}»: not a VPath")]
    BadPrefix(String),
}

/// A rule's action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    /// Allows without asking.
    Allow,
    /// Requires interactive approval.
    Ask,
    /// Denies.
    Deny,
}

/// A rule; absent fields are wildcards.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// `copy|move|delete|mkdir|create|set-mode`; **absent = any**, and that
    /// includes ones that arrive later: a rule with no `op` started granting
    /// `create` (0.57.0) and `set-mode` (0.60.0) the day they came to exist.
    /// Whoever wants to scope it down has to say so.
    #[serde(default)]
    pub op: Option<String>,
    /// SUBTREE prefix (segment containment, byte-exact via [`is_under`]):
    /// must be a valid `VPath` (e.g. `"file:///home/agent"`) and matches
    /// that node and its subtree — NEVER a sibling `…/agent-evil` (unlike a
    /// string prefix). Absent = any.
    #[serde(default)]
    pub path_prefix: Option<String>,
    /// Scheme (`file`/`sftp`/…); absent = any.
    #[serde(default)]
    pub scheme: Option<String>,
    /// `user|agent|plugin`; absent = any.
    #[serde(default)]
    pub actor: Option<String>,
    /// The action if the rule matches.
    pub action: RuleAction,
}

impl Rule {
    fn matches(&self, actor_kind: &str, op: PolicyOp, path: &VPath) -> bool {
        self.op.as_deref().is_none_or(|o| o == op.kind())
            && self.actor.as_deref().is_none_or(|a| a == actor_kind)
            && self.scheme.as_deref().is_none_or(|s| s == path.scheme())
            && self.path_prefix.as_deref().is_none_or(|pre| {
                // SEGMENT containment (byte-exact), not a string prefix:
                // avoids the `a`↔`ab` bug and split wire `%XX`s. An
                // unparseable prefix (validated at load) does not match,
                // conservatively.
                VPath::parse(pre).is_ok_and(|root| is_under(&root, path))
            })
    }
}

/// Policy config: ordered rules.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    /// Rules in order; the first one that matches wins (per path).
    #[serde(default, rename = "rule")]
    pub rules: Vec<Rule>,
}

/// The MOST restrictive of two decisions (`Deny > Ask > Allow`).
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
    /// Parses from TOML text and validates that every `path_prefix` is a
    /// `VPath`.
    ///
    /// # Errors
    /// [`PolicyConfigError::Toml`] if the document is invalid or has extra
    /// fields; [`PolicyConfigError::BadPrefix`] if a `path_prefix` fails to
    /// parse.
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

    /// Loads from `config_dir()/policy.toml`; absent = no rules. SYNCHRONOUS
    /// (startup): do not call from an async context without `spawn_blocking`.
    ///
    /// # Errors
    /// A read error (other than `NotFound`), or a parse/validation error.
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

    /// Decides for the op over ALL paths: for each path, the first matching
    /// rule; the result is the MOST restrictive of all of them. No rule for
    /// some path → `Deny(NoRule)` (fail-closed). Empty `paths` →
    /// `Deny(NoRule)`.
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
        // A prefix that is not a valid VPath fails at load (not silently).
        assert!(matches!(
            PolicyConfig::parse("[[rule]]\npath_prefix=\"not-a-vpath\"\naction=\"allow\""),
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
        // HIGH (encoding-auditor A): `file:///home/agent` does NOT cover a
        // sibling `…/agent-evil`, even though the string prefixes it.
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
            "a sibling does NOT fall under the prefix's allow"
        );
    }

    #[test]
    fn multi_path_op_takes_most_restrictive() {
        // MAJOR (security M1): a rule protecting the DESTINATION is not
        // skipped by evaluating only the source.
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
