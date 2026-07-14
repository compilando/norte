//! Resolución de un `Ask` de policy (M3-3): el engine suspende la llamada hasta
//! que un frontend aprueba/deniega. El default es `DenyAll` (headless
//! fail-closed); el daemon inyecta un resolver que difunde
//! `policy.approval_required` y await-ea `policy.decide` (M3-3b).

use async_trait::async_trait;

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

/// Resuelve un `Ask`. Implementaciones: `DenyAll` (default) y el router del
/// daemon (M3-3b).
#[async_trait]
pub trait ApprovalResolver: Send + Sync {
    /// Pide aprobación y espera el veredicto.
    async fn request(&self, req: ApprovalRequest) -> ApprovalOutcome;
}

/// Deniega todo (headless fail-closed): sin frontend interactivo, un `Ask` no se
/// puede aprobar → se deniega.
pub struct DenyAll;

#[async_trait]
impl ApprovalResolver for DenyAll {
    async fn request(&self, _req: ApprovalRequest) -> ApprovalOutcome {
        ApprovalOutcome::Denied
    }
}
