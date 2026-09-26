//! [`Backend`](super::Backend)'s journal area: deciding approvals, undoing
//! (an agent session or by `seq`), and the `journal.list` timeline.

use norte_proto::{Error, TaskId};

use super::{Backend, TaskRef};

impl Backend {
    /// Resolves a pending approval (`policy.decide`, M3-3b T5).
    ///
    /// # Errors
    /// Protocol taxonomy; `Unsupported` when embedded (approvals only ever
    /// arrive over the daemon's channel, so there's nothing to decide here).
    pub async fn policy_decide(&self, approval_id: u64, approve: bool) -> Result<(), Error> {
        match self {
            Self::Embedded(_) => Err(Error::Unsupported),
            Self::Remote(r) => r.policy_decide(approval_id, approve).await,
        }
    }

    /// Undoes an agent's whole session as a Task (`policy.undo_session`,
    /// M3-4). Only makes sense against the daemon (the journal's owner).
    ///
    /// # Errors
    /// Protocol taxonomy; `Unsupported` when embedded. Since #167 embedded
    /// CAN have a journal, but undoing AN AGENT'S SESSION belongs to the
    /// daemon: agents are governed there and that's where their sessions
    /// exist.
    pub async fn undo_session(&self, session: &str) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(_) => Err(Error::Unsupported),
            Self::Remote(r) => r.undo_session(session).await.map(TaskRef::from),
        }
    }

    /// A page of the journal's timeline (`journal.list`, 0.76.0, phase 7):
    /// newest to oldest, `before_seq` exclusive.
    ///
    /// Works EMBEDDED, unlike [`Backend::undo_session`]: what that one
    /// cannot answer without a daemon is agent sessions, and this is the
    /// journal of this machine, which the embedded engine has right in
    /// front of it.
    ///
    /// # Errors
    /// Protocol taxonomy. `Unsupported` with no journal; against a daemon,
    /// `PolicyDenied` if the connection isn't human.
    pub async fn journal_list(
        &self,
        before_seq: Option<i64>,
        limit: u32,
        actor_kind: Option<&str>,
    ) -> Result<norte_proto::methods::JournalListResult, Error> {
        match self {
            Self::Embedded(engine) => {
                let limit = limit.clamp(1, norte_proto::methods::JOURNAL_LIST_MAX_PAGE);
                let entries = engine.journal_page(before_seq, limit, actor_kind).await?;
                // The SAME cursor computation as the daemon, because it's
                // the same function: two copies of this expression is what
                // the protocol review flagged, and neither could turn red
                // on its own.
                Ok(crate::journal::page_to_wire(&entries, limit))
            }
            Self::Remote(r) => r.journal_list(before_seq, limit, actor_kind).await,
        }
    }

    /// Undoes what the human did AFTER `seq` (`journal.undo_after`, 0.76.0,
    /// phase 7). The pointed-at entry stays.
    ///
    /// Also embedded, for the same reason as [`Backend::journal_list`]:
    /// undoing one's own work needs no daemon. The report reads like any
    /// other undo's.
    ///
    /// `upto_seq` is the ceiling (0.80.0): the newest thing the human saw
    /// counted. Nothing above it gets undone. `None` = no ceiling.
    ///
    /// # Errors
    /// Protocol taxonomy; `Unsupported` with no journal.
    pub async fn undo_after(&self, seq: i64, upto_seq: Option<i64>) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let (handle, _report) = engine.undo_after(seq, upto_seq).await?;
                Ok(TaskRef::from_handle(&handle))
            }
            Self::Remote(r) => r.undo_after(seq, upto_seq).await.map(TaskRef::from),
        }
    }

    /// Report for an undo Task (`policy.undo_report`, #71): what was
    /// undone, what was skipped and why, where the LIFO got blocked.
    /// A snapshot; final once the Task is terminal.
    ///
    /// # Errors
    /// Protocol taxonomy; [`Error::NotFound`], on both arms (the daemon
    /// since 0.79.0), if that id was never an undo or the ring already
    /// evicted it.
    pub async fn undo_report(
        &self,
        task_id: TaskId,
    ) -> Result<norte_proto::methods::PolicyUndoReportResult, Error> {
        match self {
            // Embedded has no actor to check: this `Backend` IS the human
            // in-process (same criterion as `rename_batch_report`).
            Self::Embedded(engine) => engine
                .undo_report(task_id)
                .map(|(_owner, r)| crate::undo::report_to_proto(r))
                .ok_or(Error::NotFound),
            Self::Remote(r) => r.undo_report(task_id).await,
        }
    }
}
