//! Resolution of a policy `Ask` (M3-3): the engine suspends the call until a
//! frontend approves/denies it. The default is `DenyAll` (headless
//! fail-closed); the daemon injects a resolver that broadcasts
//! `policy.approval_required` and awaits `policy.decide` (M3-3b).

use async_trait::async_trait;

use crate::journal::Actor;
use crate::policy::PolicyOp;

/// Description of the op to approve (a preview for the frontend).
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// Who is asking.
    pub actor: Actor,
    /// Which operation.
    pub op: PolicyOp,
    /// Paths involved (wire). May be a PREFIX of the ones the decision
    /// covers: see `paths_total`.
    pub paths: Vec<String>,
    /// How many paths the decision actually covers. A batch rename brings
    /// two per step and can bring thousands; `paths` is trimmed so as not to
    /// flood the notification, and this number is what stops the trimmed
    /// list from being shown to the human as if it were the whole thing.
    pub paths_total: u64,
}

/// Outcome of the approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// Approved by the human.
    Approved,
    /// Denied.
    Denied,
    /// TTL expired with no decision.
    TimedOut,
}

/// Resolves an `Ask`. Implementations: `DenyAll` (default) and the daemon's
/// router (M3-3b).
#[async_trait]
pub trait ApprovalResolver: Send + Sync {
    /// Requests approval and waits for the verdict.
    async fn request(&self, req: ApprovalRequest) -> ApprovalOutcome;
}

/// Denies everything (headless fail-closed): with no interactive frontend, an
/// `Ask` cannot be approved → it is denied.
pub struct DenyAll;

#[async_trait]
impl ApprovalResolver for DenyAll {
    async fn request(&self, _req: ApprovalRequest) -> ApprovalOutcome {
        ApprovalOutcome::Denied
    }
}
