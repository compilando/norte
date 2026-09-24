//! The protocol catalogue: what methods exist and what shape they have.
//!
//! # Why it exists
//!
//! A new method touches many places: the constant and its types here, the
//! daemon's dispatch, the remote client, the notification routes, the
//! embedded and the remote backend, the schema, the goldens, MCP or the
//! frontends, and the N/N-1 compatibility window. None of those places is
//! superfluous, and the daemon's flat dispatch is deliberate — the problem
//! was never that there were many surfaces, but that **forgetting one went
//! unnoticed**.
//!
//! This is the declarative source that can be checked against. It does not
//! generate the handlers, the daemon's bodies, or the policy: it generates
//! the LIST, and the tests use it to ask each surface whether it is there.
//!
//! # What the catalogue does NOT say
//!
//! It carries no access (human/agent) and nothing about policy. That is
//! deliberate: an access field here would be a second source of truth about
//! who can call what, and one nobody consults lies the moment the first one
//! changes. Who decides that is the daemon, in the same place as always. Once
//! there is a test that checks REAL access against what is declared, then it
//! will be worth declaring it.
//!
//! # Where the name lives
//!
//! The constant stays where it is, with its documentation — which in this
//! protocol is the explanation of why each method is the way it is, and runs
//! to thousands of lines. The catalogue NAMES it, does not redeclare it:
//! putting it inside a macro would hide exactly what needs reading. That the
//! two never drift apart is guaranteed by a test: a method constant that is
//! not here turns the gate red.

use crate::methods;

/// Whether the method is STARTED by the caller or sent by the daemon on its
/// own.
///
/// `non_exhaustive` from the start: the catalogue is going to grow (human/agent
/// access enters as soon as there is a test that verifies it), and adding a
/// variant or a field later would be an API break that `cargo-semver-checks`
/// flags. Adding it now costs nothing; adding it later, a major.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
    /// Request with a response: carries an id and gets answered.
    Request,
    /// Notification: no id, no response. Sent by the daemon.
    Notification,
}

/// How the method delivers what it produces.
///
/// On a NOTIFICATION, `Direct` does not mean "answers": it means "in one
/// shot", as opposed to `Stream`, "in batches". A notification answers
/// nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Shape {
    /// In one shot: answers in the result itself (or, if a notification, one
    /// arrives and that's it).
    Direct,
    /// Returns a `task_id` and the work continues over `task.progress`.
    Task,
    /// The result arrives progressively through targeted notifications.
    Stream,
    /// The handshake, which is none of the three.
    Handshake,
}

/// One entry of the catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MethodInfo {
    /// The wire name, taken from its constant.
    pub name: &'static str,
    /// Request or notification.
    pub kind: Kind,
    /// How it delivers what it produces.
    pub shape: Shape,
    /// Name of the params type, or `"()"` if it carries none.
    pub params_ty: &'static str,
    /// Name of the result type, or `"()"` if it carries none.
    pub result_ty: &'static str,
    /// For a [`Shape::Stream`], the notifications it delivers through. Empty
    /// for everything else.
    ///
    /// This is what ties `Stream` down: without this, the difference from
    /// `Task` was "has its own notification" and the catalogue did not say
    /// which one, so nobody could disprove it. By naming them, a test checks
    /// that they exist, that they are catalogued, and that they really are
    /// notifications.
    pub stream_notifs: &'static [&'static str],
}

impl MethodInfo {
    /// The params type, if it carries one.
    #[must_use]
    pub fn params(&self) -> Option<&'static str> {
        (self.params_ty != "()").then_some(self.params_ty)
    }

    /// The result type, if it carries one.
    #[must_use]
    pub fn result(&self) -> Option<&'static str> {
        (self.result_ty != "()").then_some(self.result_ty)
    }
}

/// Declares the catalogue, and makes the compiler check the types.
///
/// Type names are stored as strings — a `&'static [MethodInfo]` cannot carry
/// types — and that would leave them unchecked. The function below fixes it:
/// it mentions every one, so a misspelled name does not compile. Without it
/// the catalogue would be a wish list.
macro_rules! rpc_catalog {
    ($(
        $konst:ident, $kind:ident, $shape:ident, $params:ty, $result:ty
        $(, [$($notif:ident),* $(,)?])? ;
    )*) => {
        /// Every method of the protocol, in the order they were declared.
        pub const CATALOG: &[MethodInfo] = &[
            $(
                MethodInfo {
                    name: methods::$konst,
                    kind: Kind::$kind,
                    shape: Shape::$shape,
                    params_ty: stringify!($params),
                    result_ty: stringify!($result),
                    stream_notifs: &[$($(methods::$notif),*)?],
                }
            ),*
        ];

        /// The types the catalogue names EXIST. Never called.
        #[allow(dead_code, clippy::used_underscore_items)]
        fn _the_types_exist() {
            $(
                let _: Option<$params> = None;
                let _: Option<$result> = None;
            )*
        }
    };
}

rpc_catalog! {
    // The handshake, mandatory before anything else (ADR 0011).
    INITIALIZE, Request, Handshake, methods::InitializeParams, methods::InitializeResult;
    DAEMON_SHUTDOWN, Request, Direct, methods::DaemonShutdownParams, methods::DaemonShutdownResult;

    // Filesystem reads.
    FS_LIST, Request, Direct, methods::FsListParams, methods::FsListResult;
    FS_STAT, Request, Direct, methods::FsStatParams, methods::FsStatResult;
    FS_READ, Request, Direct, methods::FsReadParams, methods::FsReadResult;
    FS_CAPABILITIES, Request, Direct, methods::FsCapabilitiesParams, methods::FsCapabilitiesResult;

    // Mutations: all Task, all through the journal.
    FS_COPY, Request, Task, methods::FsCopyParams, methods::FsTaskResult;
    FS_MOVE, Request, Task, methods::FsMoveParams, methods::FsTaskResult;
    FS_DELETE, Request, Task, methods::FsDeleteParams, methods::FsTaskResult;
    FS_MKDIR, Request, Task, methods::FsMkdirParams, methods::FsTaskResult;
    FS_CREATE, Request, Task, methods::FsCreateParams, methods::FsTaskResult;
    FS_SET_MODE, Request, Task, methods::FsSetModeParams, methods::FsTaskResult;

    // Search and compare: a Task that delivers by targeted notification.
    FS_SEARCH, Request, Stream, methods::FsSearchParams, methods::FsTaskResult, [SEARCH_HITS];
    SEARCH_HITS, Notification, Stream, methods::SearchHits, ();
    FS_COMPARE, Request, Stream, methods::FsCompareParams, methods::FsTaskResult, [COMPARE_ROWS];
    COMPARE_ROWS, Notification, Stream, methods::CompareRowsBatch, ();

    // Counting, checksums and their reports.
    FS_DIR_SIZE, Request, Task, methods::FsDirSizeParams, methods::FsTaskResult;
    FS_CHECKSUM, Request, Task, methods::FsChecksumParams, methods::FsTaskResult;
    FS_CHECKSUM_REPORT, Request, Direct, methods::FsChecksumReportParams, methods::FsChecksumReportResult;
    FS_DIR_USAGE, Request, Task, methods::FsDirUsageParams, methods::FsTaskResult;
    FS_DIR_USAGE_REPORT, Request, Direct, methods::FsDirUsageReportParams, methods::FsDirUsageReportResult;

    // Index and semantics.
    // Task, not Direct: the daemon answers `FsTaskResult { task_id }` and the
    // `IndexBuildResult` is the OUTCOME, which today does not travel on the
    // wire either.
    INDEX_BUILD, Request, Task, methods::IndexBuildParams, methods::FsTaskResult;
    INDEX_QUERY, Request, Direct, methods::IndexQueryParams, methods::IndexQueryResult;
    INDEX_EMBED, Request, Task, methods::IndexEmbedParams, methods::FsTaskResult;
    INDEX_SEARCH_SEMANTIC, Request, Direct, methods::IndexSearchSemanticParams, methods::IndexSearchSemanticResult;

    // Renaming: a model proposes the plan, the core executes the batch.
    AI_RENAME_PLAN, Request, Direct, methods::AiRenamePlanParams, methods::AiRenamePlanResult;
    FS_RENAME_BATCH_PLAN, Request, Direct, methods::FsRenameBatchPlanParams, methods::FsRenameBatchPlanResult;
    FS_RENAME_BATCH, Request, Task, methods::FsRenameBatchParams, methods::FsTaskResult;
    FS_RENAME_BATCH_REPORT, Request, Direct, methods::FsRenameBatchReportParams, methods::FsRenameBatchReportResult;

    // Archives: pack, test, split and combine.
    ARCHIVE_PACK, Request, Task, methods::ArchivePackParams, methods::FsTaskResult;
    ARCHIVE_PACK_REPORT, Request, Direct, methods::ArchivePackReportParams, methods::ArchivePackReportResult;
    ARCHIVE_TEST, Request, Task, methods::ArchiveTestParams, methods::FsTaskResult;
    ARCHIVE_TEST_REPORT, Request, Direct, methods::ArchiveTestReportParams, methods::ArchiveTestResult;
    FILE_SPLIT, Request, Task, methods::FileSplitParams, methods::FsTaskResult;
    FILE_COMBINE, Request, Task, methods::FileCombineParams, methods::FsTaskResult;

    // Sync: the plan stays RETAINED under the connection's name.
    SYNC_PLAN, Request, Stream, methods::SyncPlanParams, methods::FsTaskResult, [SYNC_STEPS, SYNC_PLAN_DONE];
    SYNC_STEPS, Notification, Stream, methods::SyncStepsBatch, ();
    SYNC_PLAN_DONE, Notification, Stream, methods::SyncPlanDone, ();
    SYNC_APPLY, Request, Task, methods::SyncApplyParams, methods::FsTaskResult;
    SYNC_REPORT, Request, Direct, methods::SyncReportParams, methods::SyncReportResult;

    // Tasks.
    TASK_LIST, Request, Direct, methods::TaskListParams, methods::TaskListResult;
    TASK_CANCEL, Request, Direct, methods::TaskCancelParams, methods::TaskCancelResult;
    TASK_PAUSE, Request, Direct, methods::TaskPauseParams, methods::TaskPauseResult;
    TASK_RESUME, Request, Direct, methods::TaskPauseParams, methods::TaskPauseResult;
    TASK_MOVE, Request, Direct, methods::TaskMoveParams, methods::TaskMoveResult;
    TASK_PROGRESS, Notification, Stream, crate::TaskProgress, ();
    // NOTIFICATION, not a request: it goes with no id and no response, and an
    // N-1 daemon that does not know it discards it silently (ADR 0004).
    // Sending it as a request would mean waiting for an answer that never
    // arrives.
    RPC_CANCEL, Notification, Direct, methods::RpcCancelParams, ();

    // Connections.
    HOST_VOLUMES, Request, Direct, methods::HostVolumesParams, methods::HostVolumesResult;
    CONNECTION_LIST, Request, Direct, (), methods::ConnectionListResult;
    CONNECTION_CLOSE, Request, Direct, methods::ConnectionCloseParams, methods::ConnectionCloseResult;
    CONNECTION_TRUST_HOST_KEY, Request, Direct, methods::ConnectionTrustHostKeyParams, methods::ConnectionTrustHostKeyResult;
    CONNECTION_PROVIDE_SECRET, Request, Direct, methods::ConnectionProvideSecretParams, methods::ConnectionProvideSecretResult;
    CONNECTION_DEGRADED, Notification, Direct, methods::ConnectionDegraded, ();
    CONNECTION_FAILED, Notification, Direct, methods::ConnectionFailed, ();
    DAEMON_GOING_AWAY, Notification, Direct, methods::DaemonGoingAway, ();

    // Policy: human governance of what an agent asks for.
    POLICY_REQUEST_SCOPE, Request, Direct, methods::RequestScopeParams, methods::RequestScopeResult;
    POLICY_GRANT_SCOPE, Request, Direct, methods::GrantScopeParams, methods::GrantScopeResult;
    POLICY_DECIDE, Request, Direct, methods::PolicyDecideParams, methods::PolicyDecideResult;
    POLICY_PENDING, Request, Direct, (), methods::PolicyPendingResult;
    POLICY_APPROVAL_REQUIRED, Notification, Direct, methods::PolicyApprovalRequired, ();
    POLICY_UNDO_SESSION, Request, Task, methods::PolicyUndoSessionParams, methods::PolicyUndoSessionResult;
    POLICY_UNDO_REPORT, Request, Direct, methods::PolicyUndoReportParams, methods::PolicyUndoReportResult;

    // The timeline (phase 7): read the journal, and undo up to a point in it.
    // The undo returns a Task and reports through `POLICY_UNDO_REPORT`, the
    // one above: it is the same undo with a different selection criterion.
    JOURNAL_LIST, Request, Direct, methods::JournalListParams, methods::JournalListResult;
    JOURNAL_UNDO_AFTER, Request, Task, methods::JournalUndoAfterParams, methods::PolicyUndoSessionResult;

    // Organize (phase 8): the plan is proposed by a model or a plugin, and the
    // SAME plan is applied by `fs.organize` — creating the directories and
    // moving, under a single `batch_id`, so it undoes as one unit.
    AI_ORGANIZE_PLAN, Request, Direct, methods::AiOrganizePlanParams, methods::AiOrganizePlanResult;
    PLUGIN_ORGANIZE_PLAN, Request, Direct, methods::PluginOrganizePlanParams, methods::AiOrganizePlanResult;
    FS_ORGANIZE, Request, Task, methods::FsOrganizeParams, methods::FsTaskResult;

    // Extensions.
    PLUGIN_LIST, Request, Direct, methods::PluginListParams, methods::PluginListResult;
    PLUGIN_SET_APPROVAL, Request, Direct, methods::PluginSetApprovalParams, methods::PluginSetApprovalResult;
    PLUGIN_SET_ENABLED, Request, Direct, methods::PluginSetEnabledParams, methods::PluginSetEnabledResult;
    PLUGIN_UNINSTALL, Request, Direct, methods::PluginUninstallParams, methods::PluginUninstallResult;
    PLUGIN_RUN_COMMAND, Request, Direct, methods::PluginRunCommandParams, methods::PluginRunCommandResult;
    PLUGIN_PREVIEW, Request, Direct, methods::PluginPreviewParams, methods::PluginPreviewResult;
    PLUGIN_PREVIEW_STYLED, Request, Direct, methods::PluginPreviewStyledParams, methods::PluginPreviewStyledResult;
    PLUGIN_THUMBNAIL, Request, Direct, methods::PluginThumbnailParams, methods::PluginThumbnailResult;
    PLUGIN_PANEL_RENDER, Request, Direct, methods::PluginPanelRenderParams, methods::PluginPanelRenderResult;
    PLUGIN_DECORATE, Request, Direct, methods::PluginDecorateParams, methods::PluginDecorateResult;
    PLUGIN_COLUMN_VALUES, Request, Direct, methods::PluginColumnValuesParams, methods::PluginColumnValuesResult;
    PLUGIN_RENAME_PLAN, Request, Direct, methods::PluginRenamePlanParams, methods::AiRenamePlanResult;
    PLUGIN_GET_CONFIG, Request, Direct, methods::PluginGetConfigParams, methods::PluginGetConfigResult;
    PLUGIN_SET_CONFIG, Request, Direct, methods::PluginSetConfigParams, methods::PluginSetConfigResult;
    PLUGIN_HELP, Request, Direct, methods::PluginHelpParams, methods::PluginHelpResult;
    PLUGIN_NOTICE, Notification, Direct, methods::PluginNotice, ();

    // The window's session.
    SESSION_GET, Request, Direct, (), methods::SessionGetResult;
    SESSION_PUT, Request, Direct, methods::SessionPutParams, methods::SessionPutResult;
    SESSION_RELEASE, Request, Direct, (), methods::SessionReleaseResult;

    // The daemon's log, which a frontend running as a separate process cannot
    // otherwise see. `Direct`, not `Stream`: it is PULLED with a cursor, so
    // there is no notification for it to deliver through (ADR 0092).
    LOG_TAIL, Request, Direct, methods::LogTailParams, methods::LogTailResult;
    LOG_LEVEL, Request, Direct, methods::LogLevelParams, methods::LogLevelResult;
}

/// The entry of a method by its wire name.
#[must_use]
pub fn search(name: &str) -> Option<&'static MethodInfo> {
    CATALOG.iter().find(|m| m.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No repeated name: two entries with the same name would make `search`
    /// answer the first and leave the other one findable by nobody.
    #[test]
    fn names_are_not_repeated() {
        let mut seen = std::collections::BTreeSet::new();
        for m in CATALOG {
            assert!(seen.insert(m.name), "repeated name: {}", m.name);
        }
    }

    /// A notification carries no result: there is nobody to answer it to.
    #[test]
    fn a_notification_has_no_result() {
        for m in CATALOG {
            if m.kind == Kind::Notification {
                assert_eq!(
                    m.result(),
                    None,
                    "{} is a notification and carries a result",
                    m.name
                );
            }
        }
    }

    /// **A `Stream` names where it delivers, and what it names is a catalogued
    /// notification.**
    ///
    /// This is what makes `Stream` disprovable. Before, the difference from
    /// `Task` was "has its own notification" and the catalogue did not say
    /// which one: anything could be declared `Stream` and nothing turned red.
    #[test]
    fn a_stream_names_its_notification_and_it_exists() {
        for m in CATALOG {
            if m.shape == Shape::Stream && m.kind == Kind::Request {
                assert!(
                    !m.stream_notifs.is_empty(),
                    "{} says Stream and does not say where it delivers",
                    m.name
                );
            }
            for n in m.stream_notifs {
                let Some(info) = search(n) else {
                    panic!("{} names `{n}`, which is not in the catalogue", m.name);
                };
                assert_eq!(
                    info.kind,
                    Kind::Notification,
                    "{} delivers through `{n}`, which is not a notification",
                    m.name
                );
            }
        }
    }

    /// And whatever is not `Stream` names none: a `Task` that claimed to
    /// deliver through a notification would be lying about its shape.
    #[test]
    fn only_a_stream_names_notifications() {
        for m in CATALOG {
            if m.shape != Shape::Stream {
                assert!(
                    m.stream_notifs.is_empty(),
                    "{} is {:?} and names stream notifications",
                    m.name,
                    m.shape
                );
            }
        }
    }

    /// And `search` finds what is there.
    #[test]
    fn search_finds_by_wire_name() {
        assert_eq!(
            search(methods::FS_STAT).map(|m| m.shape),
            Some(Shape::Direct)
        );
        assert_eq!(search(methods::FS_COPY).map(|m| m.shape), Some(Shape::Task));
        assert_eq!(search("fs.does_not_exist"), None);
    }
}
