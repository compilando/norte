//! The bridge: MCP (JSON-RPC 2.0 NDJSON over stdio) ↔ norte protocol (UDS).
//!
//! Rule 9: there are NO policy decisions or FS access here — every tool is a
//! 1:1 forward to the daemon, which governs (scope, ask, journal, actor)
//! server-side. The bridge is just another agent client: compromising it
//! does not skip policy. The MCP wire types are built with `serde_json::json!`
//! (they are NOT norte-proto's `Response`: they only share the NDJSON framing).

use std::sync::Arc;

use base64::Engine as _;
use norte_core::backend::Backend;
use norte_core::backend::remote::RemoteBackend;
use norte_core::daemon::{Client, ClientError};
use norte_proto::methods;
use norte_proto::{ByteRange, DeleteMode, VPath};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// The MCP version we answer with, fixed (ADR 0024).
const MCP_VERSION: &str = "2025-06-18";
/// Wait cap for the terminal state of a Task a mutating tool queued.
/// Generous: an op under an `ask` rule ALREADY waited for its approval
/// INSIDE the daemon call; this only covers execution.
const TASK_WAIT: std::time::Duration = std::time::Duration::from_mins(10);
/// Poll interval for `task.list` while waiting for the terminal.
const TASK_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// `compare` rows per call if the caller does not ask for `limit` (~2 batches
/// of [`norte_proto::methods::COMPARE_ROWS_MAX_BATCH`]). `fs.compare` does
/// not paginate like `fs.list` (no `cursor`, and the walk has no `max_hits`
/// like `fs.search`): without this cap, comparing two large trees would put
/// a million rows in a single tool result and blow up the model's context.
const COMPARE_ROWS_DEFAULT: usize = 500;
/// HARD cap on `limit`, even if the caller asks for more: an uncapped
/// `limit` would be the same problem as no cap, with an extra step.
const COMPARE_ROWS_MAX: usize = 5000;

/// `sync_plan` steps per call if the caller does not ask for `limit`. SAME
/// value as [`COMPARE_ROWS_DEFAULT`] and the same reason: `sync.plan` does
/// not paginate either (no `cursor`), and a plan over two large trees would
/// put hundreds of thousands of steps in a single tool result. The payload's
/// name and vocabulary (`limit`/`truncated`/`complete`) are deliberately the
/// same as `compare`'s: a model reading both tools should not have to learn
/// two vocabularies for the same idea.
const SYNC_STEPS_DEFAULT: usize = 500;
/// HARD cap on `sync_plan`'s `limit`, for the same reason as
/// [`COMPARE_ROWS_MAX`].
const SYNC_STEPS_MAX: usize = 5000;

/// Cell for the JSON-RPC id of a tool's in-flight mutating `fs.*` (#72).
type DaemonIdCell = Arc<std::sync::OnceLock<u64>>;

/// Errors from the bridge's lifecycle (connection/transport). A TOOL's
/// errors do not reach here: they travel as `isError: true` in the MCP
/// result (the agent can read them and react).
///
/// `non_exhaustive`: the list grows with every new bridge surface (`Streams`
/// introduced it) and none of those additions should be a break.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BridgeError {
    /// stdio I/O.
    #[error("stdio: {0}")]
    Io(#[from] std::io::Error),
    /// Failure talking to the daemon (connection/handshake).
    #[error("daemon: {0}")]
    Daemon(#[from] ClientError),
    /// Failure opening the streams arm ([`Bridge::streams`]). Separate from
    /// [`Self::Daemon`] because it arrives in the protocol's taxonomy, not
    /// as a `ClientError`, and because it distinguishes "the bridge did not
    /// start" from "a tool could not open its second connection".
    #[error("streams arm: {0}")]
    Streams(#[source] norte_proto::Error),
}

/// The bridge connected to the daemon as an AGENT SESSION.
pub struct Bridge {
    client: Client,
    session: String,
    /// The socket, kept so [`Bridge::streams`] can be opened on the fly.
    socket: std::path::PathBuf,
    /// The connection that drains notifications, opened on the FIRST tool
    /// that needs it.
    ///
    /// Lazy on purpose: an agent that only lists and reads never opens it,
    /// and a second connection to the daemon is not free. A single one,
    /// cached: two would be two `conn_id`s with no advantage.
    streams: tokio::sync::OnceCell<Backend>,
}

impl Bridge {
    /// Connects to the daemon over `socket` and negotiates the handshake
    /// declaring `agent_session = session`: every mutation from this bridge
    /// stays tied to that actor server-side.
    ///
    /// # Errors
    /// [`BridgeError::Daemon`]: unreachable socket, incompatible version, or
    /// rejected session (charset `[A-Za-z0-9._-]`, 1..=64).
    pub async fn connect(socket: &std::path::Path, session: &str) -> Result<Self, BridgeError> {
        let mut client = Client::connect(socket).await?;
        // The SAME handshake the streams arm uses
        // (`RemoteBackend::connect_as_agent` calls here too). There used to
        // be a literal `InitializeParams` here, with a comment justifying it
        // by saying the bridge keeps the `InitializeResult`: it did not keep
        // it —it dropped it into `_init`—, and the real reason was that the
        // method was `pub(crate)`. Two literals for the two halves of ONE
        // agent session is how an expanded `encodings` reaches one and not
        // the other.
        let _init: methods::InitializeResult = client
            .initialize_as_agent(client_info(), session.to_owned())
            .await?;
        Ok(Self {
            client,
            session: session.to_owned(),
            socket: socket.to_path_buf(),
            streams: tokio::sync::OnceCell::new(),
        })
    }

    /// The arm that drains notifications, opening it the first time.
    ///
    /// `fs.compare` and `sync.plan` do not answer with their result: they
    /// deliver it via notifications (`compare.rows`, `sync.steps`), and this
    /// bridge's [`Client`] does not route them — its channel is taken with
    /// `&mut self` and, with eight tools in flight, they would need to be
    /// demultiplexed by `task_id`. That demultiplexer already exists in
    /// [`Backend::Remote`], so the bridge opens a SECOND connection to the
    /// daemon and uses it for those two methods.
    ///
    /// It is the SAME actor: it is opened with `connect_as_agent(self.session)`,
    /// and policy scopes are indexed by SESSION
    /// (`ScopeRegistry::grant(session, …)`), not by connection — what was
    /// granted to the agent is worth the same here. What is NOT shared is
    /// the `conn_id`: a plan retained in the spool for this connection is
    /// not redeemable from the tools' one, which is exactly why the bridge
    /// does not offer `sync_apply`.
    ///
    /// Lazy and cached: it is opened once and, on the normal path, dies with
    /// the bridge (the `Backend` is a field, not a `spawn`; when it is
    /// dropped, its notification pump sees the last `Arc` fall and exits on
    /// its own). "Normal" is literal: `Backend` is `Clone` and every
    /// `TaskRef` that comes out of here carries a clone inside, so a
    /// retained clone —or a live task— keeps it open past the bridge. Do not
    /// hold onto it.
    ///
    /// **No logical operation can be split across the two connections**
    /// (check on one and act on the other). Between the two calls, scope
    /// state and even the daemon can change: this arm RECONNECTS on its own
    /// and the tools connection does not, so after a restart the arm can be
    /// talking to a new daemon —empty `ScopeRegistry`— while the other one
    /// is dead. Both sides fail closed, but the race exists: each tool
    /// commits to ONE connection.
    ///
    /// # Tests only
    /// `pub` only because `norte-mcp`'s E2E needs to check that the arm
    /// opens ONCE; it is not stable API (`doc(hidden)`, can change without a
    /// bump), and the precedent in the tree is
    /// `norte_core::backend::TaskRef::synthetic_for_tests`. A caller that
    /// uses it has the agent's authenticated connection's WHOLE [`Backend`] —
    /// `sync_apply` included, whose absence is exactly what ADR 0050
    /// decides. Tools go through here; nobody else should.
    ///
    /// # Errors
    /// [`BridgeError::Streams`] if the daemon does not accept the second
    /// connection (socket down, session rejected).
    #[doc(hidden)]
    pub async fn streams(&self) -> Result<&Backend, BridgeError> {
        self.streams
            .get_or_try_init(|| async {
                let remote = RemoteBackend::connect_as_agent(
                    self.socket.clone(),
                    client_info(),
                    self.session.clone(),
                )
                .await
                .map_err(BridgeError::Streams)?;
                let mut backend = Backend::Remote(remote);
                // The daemon broadcasts to this connection the progress of
                // its SAME session's tasks —i.e. the tools connection's—,
                // and the backend queues them as "foreign" in an uncapped
                // channel. The bridge does not look at them (each tool
                // follows its own task via `task.list`), so the receiver is
                // dropped: without it, `send` is a no-op and the queue does
                // not grow for the process's whole lifetime.
                let _ = backend.take_foreign_tasks();
                Ok(backend)
            })
            .await
    }

    /// Processes ONE line of the MCP transport and returns the already
    /// serialized response (`None` for notifications — MCP 2025-06-18 has no
    /// batches, so a request produces EXACTLY one response). Separate from
    /// stdio so tests can drive it without a process.
    pub async fn handle_line(&self, line: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                return Some(rpc_error(
                    &Value::Null,
                    -32700,
                    &format!("parse error: {e}"),
                ));
            }
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        // MCP notifications (no id): initialized/cancelled/… — no response.
        let id = id?;
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let out = match method {
            "initialize" => rpc_result(
                &id,
                &json!({
                    "protocolVersion": MCP_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {
                        "name": "norte-mcp",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                }),
            ),
            "ping" => rpc_result(&id, &json!({})),
            "tools/list" => rpc_result(&id, &json!({"tools": tool_defs()})),
            "tools/call" => self.tools_call(&id, &params, &Arc::default()).await,
            other => rpc_error(&id, -32601, &format!("unknown method: {other}")),
        };
        Some(out)
    }

    /// A complete `tools/call`: runs the tool and returns the serialized MCP
    /// response. It is the unit the concurrent transport (#67) dispatches to
    /// its own task.
    pub async fn tools_call(&self, id: &Value, params: &Value, daemon_id: &DaemonIdCell) -> String {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        match self.call_tool(name, &args, daemon_id).await {
            Ok(v) => rpc_result(id, &tool_content(&v, false)),
            // TOOL error: the agent READS it (isError) and reacts — e.g.
            // requesting scope after an out-of-scope.
            Err(text) => rpc_result(id, &tool_content(&json!(text), true)),
        }
    }

    /// Dispatches a tool to its wire method. `Err(text)` = tool failure
    /// (travels as `isError`, never breaks the transport).
    async fn call_tool(
        &self,
        name: &str,
        args: &Value,
        daemon_id: &DaemonIdCell,
    ) -> Result<Value, String> {
        match name {
            "list_dir" => self.tool_list_dir(args).await,
            "stat" => self.tool_stat(args).await,
            "read_file" => self.tool_read_file(args).await,
            "copy" | "move" => self.tool_transfer(name, args, daemon_id).await,
            "delete" => self.tool_delete(args, daemon_id).await,
            "task_status" => self.tool_task_status(args).await,
            "request_scope" => self.tool_request_scope(args).await,
            "compare" => self.tool_compare(args).await,
            "sync_plan" => self.tool_sync_plan(args).await,
            other => Err(format!("unknown tool: {other}")),
        }
    }

    async fn tool_list_dir(&self, args: &Value) -> Result<Value, String> {
        let path = vpath_arg(args, "path")?;
        let limit = opt_u64_arg(args, "limit")?
            .map(|l| u32::try_from(l).map_err(|_| format!("arg limit too large: {l}")))
            .transpose()?;
        let cursor = args
            .get("cursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let r: methods::FsListResult = self
            .call(
                methods::FS_LIST,
                &methods::FsListParams {
                    path,
                    limit,
                    cursor,
                    // The bridge does not expose provider attrs (ADR 0039,
                    // block 1 = wire only): empty = none delivered.
                    attrs: Vec::new(),
                },
            )
            .await?;
        let entries: Vec<Value> = r
            .entries
            .iter()
            .map(|e| {
                json!({
                    "path": e.path.to_wire(),
                    "kind": e.kind,
                    "size": e.size,
                    "mtime_ms": e.mtime_ms,
                })
            })
            .collect();
        Ok(json!({"entries": entries, "next_cursor": r.next_cursor}))
    }

    async fn tool_stat(&self, args: &Value) -> Result<Value, String> {
        let path = vpath_arg(args, "path")?;
        let r: methods::FsStatResult = self
            .call(
                methods::FS_STAT,
                &methods::FsStatParams {
                    path,
                    attrs: Vec::new(),
                },
            )
            .await?;
        Ok(json!({
            "path": r.entry.path.to_wire(),
            "kind": r.entry.kind,
            "size": r.entry.size,
            "mtime_ms": r.entry.mtime_ms,
        }))
    }

    async fn tool_read_file(&self, args: &Value) -> Result<Value, String> {
        let path = vpath_arg(args, "path")?;
        let offset = opt_u64_arg(args, "offset")?.unwrap_or(0);
        let len = opt_u64_arg(args, "len")?;
        let r: methods::FsReadResult = self
            .call(
                methods::FS_READ,
                &methods::FsReadParams {
                    path,
                    range: Some(ByteRange { offset, len }),
                },
            )
            .await?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&r.content_b64)
            .map_err(|e| format!("daemon sent invalid base64: {e}"))?;
        // Text if it IS one; if not, marked lossy + the faithful bytes in
        // base64 (the agent chooses what to look at; nothing is lost).
        match String::from_utf8(bytes) {
            Ok(text) => Ok(json!({"text": text, "eof": r.eof})),
            Err(e) => {
                let bytes = e.into_bytes();
                Ok(json!({
                    "text": String::from_utf8_lossy(&bytes),
                    "base64": r.content_b64,
                    "eof": r.eof,
                }))
            }
        }
    }

    /// `copy` and `move` share a body but each serializes ITS OWN params
    /// type (rust-reviewer M3: reusing `FsCopyParams` for `fs.move` worked
    /// by shape coincidence — an invisible future divergence).
    async fn tool_transfer(
        &self,
        name: &str,
        args: &Value,
        daemon_id: &DaemonIdCell,
    ) -> Result<Value, String> {
        let from = vpath_arg(args, "from")?;
        let to = vpath_arg(args, "to")?;
        let r: methods::FsTaskResult = if name == "copy" {
            self.call_tracked(
                methods::FS_COPY,
                &methods::FsCopyParams {
                    from,
                    to,
                    on_collision: norte_proto::CollisionPolicy::default(),
                    symlinks: norte_proto::SymlinkPolicy::default(),
                    resume: norte_proto::ResumePolicy::default(),
                    verify: norte_proto::VerifyPolicy::default(),
                    // An agent does not anchor its destination (#295): the
                    // anchor says what a HUMAN was looking at when they
                    // approved, and there is no human listing behind this.
                    // What bounds an agent is its policy scope, which is a
                    // different thing and still applies.
                    dest_anchor: None,
                    queued: false,
                },
                daemon_id,
            )
            .await?
        } else {
            self.call_tracked(
                methods::FS_MOVE,
                &methods::FsMoveParams {
                    from,
                    to,
                    on_collision: norte_proto::CollisionPolicy::default(),
                    symlinks: norte_proto::SymlinkPolicy::default(),
                    resume: norte_proto::ResumePolicy::default(),
                    verify: norte_proto::VerifyPolicy::default(),
                    // No anchor, for the same reason as the copy above.
                    dest_anchor: None,
                    queued: false,
                },
                daemon_id,
            )
            .await?
        };
        self.wait_terminal(r.task_id).await
    }

    async fn tool_delete(&self, args: &Value, daemon_id: &DaemonIdCell) -> Result<Value, String> {
        let path = vpath_arg(args, "path")?;
        let mode = match args.get("mode") {
            // Default TRASH (spec §10): an agentic delete is ALWAYS
            // recoverable unless explicitly requested (which policy can
            // still deny). A `mode` PRESENT with an illegal type/value is an
            // error — never degrade silently (sec MINOR-1).
            None | Some(Value::Null) => DeleteMode::Trash,
            Some(Value::String(s)) if s == "trash" => DeleteMode::Trash,
            Some(Value::String(s)) if s == "permanent" => DeleteMode::Permanent,
            Some(other) => {
                return Err(format!("invalid mode {other}: use \"trash\"|\"permanent\""));
            }
        };
        let r: methods::FsTaskResult = self
            .call_tracked(
                methods::FS_DELETE,
                &methods::FsDeleteParams { path, mode },
                daemon_id,
            )
            .await?;
        self.wait_terminal(r.task_id).await
    }

    async fn tool_task_status(&self, args: &Value) -> Result<Value, String> {
        let id = args
            .get("task_id")
            .and_then(Value::as_u64)
            .ok_or("missing integer arg: task_id")?;
        let r: methods::TaskListResult = self
            .call(methods::TASK_LIST, &methods::TaskListParams {})
            .await?;
        match r.tasks.iter().find(|t| t.task_id.get() == id) {
            Some(t) => Ok(task_json(t)),
            None => Err(format!("unknown task {id} (too old or never existed)")),
        }
    }

    async fn tool_request_scope(&self, args: &Value) -> Result<Value, String> {
        let roots = args
            .get("roots")
            .and_then(Value::as_array)
            .ok_or("missing array arg: roots")?
            .iter()
            .map(|v| {
                let s = v.as_str().ok_or("roots must be strings")?;
                VPath::parse(s).map_err(|e| format!("invalid VPath {s:?}: {e}"))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let ops = args
            .get("ops")
            .and_then(Value::as_array)
            .ok_or("missing array arg: ops")?
            .iter()
            .map(|v| v.as_str().map(str::to_owned).ok_or("ops must be strings"))
            .collect::<Result<Vec<_>, _>>()?;
        let ttl_ms = args
            .get("ttl_ms")
            .and_then(Value::as_u64)
            .ok_or("missing integer arg: ttl_ms")?;
        let r: methods::RequestScopeResult = self
            .call(
                methods::POLICY_REQUEST_SCOPE,
                &methods::RequestScopeParams {
                    // ALWAYS the bridge's session: identity is not an
                    // argument (the daemon re-validates it anyway).
                    session: self.session.clone(),
                    roots,
                    ops,
                    ttl_ms,
                },
            )
            .await?;
        Ok(json!({
            "request_id": r.request_id,
            "status": "pending human grant",
            "hint": format!(
                "a human must run `norte policy grant {}` (or grant from the TUI); retry the operation afterwards",
                r.request_id
            ),
        }))
    }

    /// `compare`: two trees, and what differs between them. Mutates NOTHING
    /// (no journal, no undo — hard rule 4 does not apply: `Backend::compare`
    /// does not write).
    ///
    /// Goes through [`Self::streams`], not [`Self::call`]: `fs.compare`
    /// delivers its rows via notification (`compare.rows`), and this tools
    /// connection does not route them (see `streams`'s rustdoc).
    /// `Backend::compare` validates equal roots and `follow_symlinks` BEFORE
    /// picking an arm, so that check comes free here.
    ///
    /// # The row cap
    /// `fs.compare` does not paginate (no `cursor`) nor has `max_hits` like
    /// `fs.search`: without a cap, comparing two large trees would put a
    /// million rows in a single tool result. On reaching `limit`
    /// ([`COMPARE_ROWS_DEFAULT`], ceiling [`COMPARE_ROWS_MAX`]) draining
    /// stops, the task is CANCELLED and the payload says `truncated: true`.
    /// A silent truncation would be worse than the cap: a model reading it
    /// as complete would report two trees as equal when it is not known.
    ///
    /// # The deadline
    /// Same cap as the rest of the tools that wait on a task ([`TASK_WAIT`]):
    /// with the `hash` rung on, a comparison can take hours —`fs.compare`'s
    /// own rustdoc says so— and would occupy one of the
    /// [`MAX_INFLIGHT_TOOLS`] slots without ever answering. Past the
    /// deadline, what was drained is returned with `timed_out: true` and
    /// `complete: false`, and the daemon's task is cancelled when the guard
    /// is dropped.
    ///
    /// # `complete`, and why the terminal state is not enough
    /// The same distinction as `norte compare`'s (the CLI command,
    /// `compare_cmd`) exit code 2, PLUS the row count. The `compare.rows`
    /// feed is routed with `OnFull::DropBatch`: a dropped batch —a full
    /// client buffer, a route teardown race, an outbox eviction— only leaves
    /// a `warn!`, the channel closes CLEANLY and the task ends `Completed`.
    /// In other words, "terminal and not truncated" does not prove all the
    /// rows are there. `TaskProgress::entries_done` counts the SENT rows and
    /// is —by `norte_core::compare`'s contract— the only signal a client can
    /// use to detect it lost a notification, so `complete` also requires it
    /// to match the received rows, and `rows_total` publishes it so the
    /// model SEES the gap instead of having to deduce it.
    ///
    /// This was pulled out once for parity with the CLI. Parity does not
    /// apply: a human reads a diff pane, and a boolean named `complete` in a
    /// schema tells a model the comparison reached the end.
    async fn tool_compare(&self, args: &Value) -> Result<Value, String> {
        let left = vpath_arg(args, "left")?;
        let right = vpath_arg(args, "right")?;
        let criteria = compare_criteria_arg(args)?;
        let max_depth = opt_u64_arg(args, "max_depth")?
            .map(|d| u32::try_from(d).map_err(|_| format!("arg max_depth too large: {d}")))
            .transpose()?;
        let mtime_tolerance_ms = opt_u64_arg(args, "mtime_tolerance_ms")?
            .map(|t| u32::try_from(t).map_err(|_| format!("arg mtime_tolerance_ms too large: {t}")))
            .transpose()?
            .unwrap_or_else(default_mtime_tolerance_ms);
        let limit = stream_limit_arg(args, COMPARE_ROWS_DEFAULT, COMPARE_ROWS_MAX)?;

        let params = methods::FsCompareParams {
            left,
            right,
            criteria,
            max_depth,
            mtime_tolerance_ms,
            // The bridge does not offer following symlinks
            // (`Backend::compare` rejects it anyway): see `tool_transfer`'s
            // rustdoc for why options the core does not support are not
            // exposed.
            follow_symlinks: false,
            descend_orphans: None,
        };

        let (task, mut rx) = self
            .streams()
            .await
            .map_err(|e| e.to_string())?
            .compare(params)
            .await
            .map_err(map_backend_err)?;
        let task_id = task.id();
        let progress = task.progress();
        // Armed BEFORE the stream's first await: if the agent sends
        // `notifications/cancelled` (or the transport dies), this tool's
        // future is DROPPED and the daemon's walk would keep reading —and
        // hashing— both whole trees. See the guard's rustdoc.
        let mut guard = CancelOnAbandon::new(task.canceller());

        // Rows faithful to the wire: `CompareRow` serializes
        // `verdict`/`criterion`/`confidence` as the protocol's snake_case
        // values (never a translated label) and its `Entry`'s paths as
        // `to_wire()` — the SAME shape `norte compare --json` already
        // exposes (rule 1; never a lossy string). Whether to truncate is
        // decided row by row instead of collecting the whole batch first: a
        // batch that exactly exhausts the remaining cap must not drag in
        // one extra row.
        let mut rows: Vec<methods::CompareRow> = Vec::new();
        let mut truncated = false;
        let drained = tokio::time::timeout(TASK_WAIT, async {
            'drain: while let Some(batch) = rx.recv().await {
                for row in batch.rows {
                    if rows.len() >= limit {
                        truncated = true;
                        break 'drain;
                    }
                    rows.push(row);
                }
            }
            if truncated {
                // Cooperative (hard rule 3): without this, a comparison of
                // millions of rows would keep reading (and hashing) both
                // whole trees for an `rx` nobody drains anymore.
                task.cancel();
            }
            task.join().await
        })
        .await;
        // Only the path that SAW the terminal disarms: the deadline's path
        // lets the guard cancel on return.
        let state = match drained {
            Ok(state) => {
                guard.disarm();
                Some(state)
            }
            Err(_) => None,
        };
        let emitted = progress.borrow().entries_done;
        let complete = stream_is_complete(truncated, state.as_ref(), rows.len(), emitted);

        let encoded = serde_json::to_value(&rows)
            .map_err(|e| format!("tool compare: could not encode rows: {e}"))?;
        Ok(json!({
            "task_id": task_id.get(),
            "rows": encoded,
            "rows_total": emitted,
            "truncated": truncated,
            "timed_out": state.is_none(),
            "state": state_label(state.as_ref()),
            "error": state_error(state.as_ref()),
            "complete": complete,
        }))
    }

    /// `sync_plan`: what a one-way synchronisation WOULD DO. Applies
    /// NOTHING — there is no `sync_apply` tool (spec 3 §2.1): applying is a
    /// HUMAN's action in their own client, and the bridge does not offer it.
    ///
    /// Goes through [`Self::streams`], same as [`Self::tool_compare`]:
    /// `sync.plan` delivers its steps via notification (`sync.steps`* and a
    /// `sync.plan_done`) and this tools connection does not route them (see
    /// [`Self::streams`]'s rustdoc).
    ///
    /// # The hash does NOT travel
    /// [`methods::SyncPlanDone::plan_hash`] stays retained ONLY for the
    /// [`Self::streams`] connection — a plan approved by this call is not
    /// redeemable from the tools connection (no tool tries it), so the hash
    /// is a value NOBODY outside this call can use. It is omitted from the
    /// payload on purpose: a value that is good for nothing is an invitation
    /// to try it anyway.
    ///
    /// The `task_id` DOES travel, and not for the hash's sake: the bridge's
    /// two connections are the SAME `Actor::Agent { session }`, and the
    /// daemon's visibility criterion is actor equality, so the tools
    /// connection can observe (`task_status`) and cancel the task the
    /// streams arm opened. Removing it left an agent with nothing to do
    /// about a plan that was taking a while.
    ///
    /// # The step cap, and the deadline
    /// Same contract as [`Self::tool_compare`] (same field names, on
    /// purpose): on reaching `limit` ([`SYNC_STEPS_DEFAULT`], ceiling
    /// [`SYNC_STEPS_MAX`]) draining stops, the task is CANCELLED and the
    /// payload says `truncated: true`; past [`TASK_WAIT`], what was drained
    /// is returned with `timed_out: true`. With the task cancelled,
    /// `sync.plan_done` NEVER arrives (`run_sync_plan` does not emit it on
    /// the error path: see `norte_core::sync::run_sync_plan`), so
    /// `counts`/`dest_trash`/`blockers`/`blockers_total`/`executable` are
    /// ABSENT from the payload — never zeroed nor invented — and that is
    /// exactly what `complete: false` warns about.
    ///
    /// # `complete` checks the steps against `counts`
    /// The `Done` is a stronger loss detector than [`Self::tool_compare`]'s
    /// —the `sync.steps` feed is routed with `OnFull::CloseFeed`, so a
    /// dropped batch closes the channel and the `Done` never arrives— but it
    /// does not cover everything: a batch that arrives BEFORE the route is
    /// registered and finds `pending` full is dropped with a trace and
    /// without closing anything, so the `Done` can show up with a step list
    /// that is not what `counts` counts. `counts` is rebuilt step by step in
    /// the spool (it is what the executor looks at to request its policy
    /// gates), so the sum of its classes IS the plan's total step count: if
    /// it does not match what was received, this is not complete. It also
    /// goes into the payload as `steps_total`.
    async fn tool_sync_plan(&self, args: &Value) -> Result<Value, String> {
        let (params, limit) = sync_plan_args(args)?;

        let (task, mut rx) = self
            .streams()
            .await
            .map_err(|e| e.to_string())?
            .sync_plan(params)
            .await
            .map_err(map_plan_err)?;
        let task_id = task.id();
        let mut guard = CancelOnAbandon::new(task.canceller());

        // Steps faithful to the wire: `SyncStep` serializes
        // `kind`/`criterion`/`confidence`/`reversal`/`reason` as the
        // protocol's snake_case values (rule 1; never a translated label)
        // and `rel`/`dest_rel` as their wire bytes.
        let mut steps: Vec<methods::SyncStep> = Vec::new();
        let mut truncated = false;
        let mut done: Option<methods::SyncPlanDone> = None;
        let drained = tokio::time::timeout(TASK_WAIT, async {
            'drain: while let Some(event) = rx.recv().await {
                match event {
                    norte_core::sync::SyncPlanEvent::Steps(batch) => {
                        for step in batch.steps {
                            if steps.len() >= limit {
                                truncated = true;
                                break 'drain;
                            }
                            steps.push(step);
                        }
                    }
                    norte_core::sync::SyncPlanEvent::Done(d) => {
                        // At most ONE per Task, and always the last — nothing
                        // left to drain afterward.
                        done = Some(d);
                        break;
                    }
                }
            }
            if truncated {
                // Cooperative (hard rule 3): same as in `tool_compare`,
                // without this the planner would keep walking (and
                // comparing) both whole trees for an `rx` nobody drains
                // anymore.
                task.cancel();
            }
            task.join().await
        })
        .await;
        let state = match drained {
            Ok(state) => {
                guard.disarm();
                Some(state)
            }
            Err(_) => None,
        };
        let steps_total = done.as_ref().map(|d| plan_steps_total(&d.counts));
        let complete = done.is_some()
            && stream_is_complete(
                truncated,
                state.as_ref(),
                steps.len(),
                steps_total.unwrap_or_default(),
            );

        let encoded = serde_json::to_value(&steps)
            .map_err(|e| format!("tool sync_plan: could not encode steps: {e}"))?;
        let mut payload = json!({
            "task_id": task_id.get(),
            "steps": encoded,
            "truncated": truncated,
            "timed_out": state.is_none(),
            "state": state_label(state.as_ref()),
            "error": state_error(state.as_ref()),
            "complete": complete,
        });
        if let (Some(d), Some(total)) = (done, steps_total) {
            merge_plan_done(&mut payload, &d, total)?;
        }
        Ok(payload)
    }

    /// The wire's `call` with RPC errors converted to tool text. The
    /// taxonomy travels in `data`: a `PolicyDenied` comes out ACTIONABLE.
    async fn call<P, R>(&self, method: &str, params: &P) -> Result<R, String>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        self.client
            .call(method, params)
            .await
            .map_err(map_client_err)
    }

    /// Like [`Self::call`], but records the assigned JSON-RPC id into
    /// `daemon_id` (a `OnceLock` cell) BEFORE suspending — so the
    /// `notifications/cancelled` handler can forward an `rpc.cancel` for
    /// that request while it is still suspended in an Ask (#72).
    async fn call_tracked<P, R>(
        &self,
        method: &str,
        params: &P,
        daemon_id: &DaemonIdCell,
    ) -> Result<R, String>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned,
    {
        self.client
            .call_tracked(method, params, |id| {
                // OnceLock: the FIRST mutating `fs.*` of this tool sets the
                // id; later task.list polls do NOT overwrite it.
                let _ = daemon_id.set(id);
            })
            .await
            .map_err(map_client_err)
    }

    /// Waits for `task_id`'s terminal state by polling `task.list`. The task
    /// was JUST ack'd by the daemon, so it exists: if a poll finds it
    /// NEITHER alive NOR among the recent outcomes, it means it finished and
    /// its outcome was EVICTED from the recent-outcomes buffer (64, global)
    /// — an immediate HONEST error is returned instead of exhausting the
    /// deadline claiming "still running" (rust-reviewer M1).
    async fn wait_terminal(&self, task_id: norte_proto::TaskId) -> Result<Value, String> {
        let deadline = tokio::time::Instant::now() + TASK_WAIT;
        loop {
            let r: methods::TaskListResult = self
                .call(methods::TASK_LIST, &methods::TaskListParams {})
                .await?;
            match r.tasks.iter().find(|t| t.task_id == task_id) {
                Some(t) if t.state.is_terminal() => return Ok(task_json(t)),
                Some(_) => {}
                None => {
                    return Err(format!(
                        "task {} finished but its outcome was evicted (busy daemon); \
                         verify the result with stat/list_dir",
                        task_id.get()
                    ));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "task {} not observed terminal within {}s; poll it with task_status",
                    task_id.get(),
                    TASK_WAIT.as_secs()
                ));
            }
            tokio::time::sleep(TASK_POLL).await;
        }
    }

    /// Forwards an `rpc.cancel` for request `daemon_id` to the daemon (#72):
    /// if that `fs.*` is still suspended in an Ask, the daemon withdraws it
    /// (fail-closed). Best-effort: an already-resolved id is a no-op on the
    /// daemon; a dead channel is dropped. The daemon governs: compromising
    /// the bridge does NOT skip policy.
    pub fn cancel_daemon_request(&self, daemon_id: u64) {
        let _ = self.client.notify(
            methods::RPC_CANCEL,
            &methods::RpcCancelParams {
                id: norte_proto::wire::RequestId::Num(daemon_id),
            },
        );
    }
}

/// Translates a `Client` error into tool text (the taxonomy is in `data`; a
/// `PolicyDenied` comes out ACTIONABLE). Shared by `call` and `call_tracked`.
fn map_client_err(e: ClientError) -> String {
    match e {
        ClientError::Rpc(rpc) => match rpc.data {
            Some(norte_proto::Error::PolicyDenied { ref rule }) => format!(
                "denied by policy ({rule}). If out-of-scope, call request_scope and ask the human to grant it."
            ),
            Some(err) => format!("{err}"),
            None => format!("rpc error {}: {}", rpc.code, rpc.message),
        },
        other => format!("daemon unreachable: {other}"),
    }
}

/// Translates a [`norte_proto::Error`] from [`Bridge::tool_compare`] (which
/// talks to [`Bridge::streams`], NOT to [`Client`], so there is no
/// `ClientError` to wrap) into the same ACTIONABLE text as
/// [`map_client_err`]: a `PolicyDenied` has to say "ask for scope" no matter
/// which arm it comes out of.
fn map_backend_err(e: norte_proto::Error) -> String {
    match e {
        norte_proto::Error::PolicyDenied { ref rule } => format!(
            "denied by policy ({rule}). If out-of-scope, call request_scope and ask the human to grant it."
        ),
        other => format!("{other}"),
    }
}

/// `sync_plan`'s args → `(params, limit)`. Split out from the tool only for
/// the length lint, and the split is the natural one: the daemon is not
/// touched here.
fn sync_plan_args(args: &Value) -> Result<(methods::SyncPlanParams, usize), String> {
    let source = vpath_arg(args, "source")?;
    let dest = vpath_arg(args, "dest")?;
    // `mode` has no neutral value between copying and deleting (same as on
    // the wire, `SyncPlanParams::mode` carries no `#[serde(default)]`):
    // ABSENT is as much an error as MALFORMED — never a silent degradation
    // (same criterion as `tool_delete::mode`, with the nuance that there is
    // no default to offer here).
    let mode = match args.get("mode") {
        Some(Value::String(s)) if s == "update" => methods::SyncMode::Update,
        Some(Value::String(s)) if s == "mirror" => methods::SyncMode::Mirror,
        None | Some(Value::Null) => {
            return Err("missing arg mode: use \"update\"|\"mirror\" (no default)".to_owned());
        }
        Some(other) => return Err(format!("invalid mode {other}: use \"update\"|\"mirror\"")),
    };
    let criteria = compare_criteria_arg(args)?;
    let on_unknown = match args.get("on_unknown") {
        None | Some(Value::Null) => methods::OnUnknown::Copy,
        Some(Value::String(s)) if s == "copy" => methods::OnUnknown::Copy,
        Some(Value::String(s)) if s == "skip" => methods::OnUnknown::Skip,
        Some(other) => return Err(format!("invalid on_unknown {other}: use \"copy\"|\"skip\"")),
    };
    let limit = stream_limit_arg(args, SYNC_STEPS_DEFAULT, SYNC_STEPS_MAX)?;
    let params = methods::SyncPlanParams {
        source,
        dest,
        mode,
        compare: methods::SyncCompareOptions {
            criteria,
            // `max_depth`/`mtime_tolerance_ms` are not arguments of this
            // tool (spec 3 §4): the wire default is enough for an agentic
            // plan, and `follow_symlinks`/`descend_orphans` are NOT the
            // caller's to set in `sync.plan` — asking for them is `-32602`
            // server-side. The tool's description SAYS so, because an agent
            // that just compared with `max_depth: 2` would otherwise get a
            // plan over the whole tree with no signal at all.
            ..methods::SyncCompareOptions::default()
        },
        on_unknown,
        // The bridge does not offer selecting a subtree of the plan: that is
        // a surface of the diff panel (spec 3 §4), not of an agent that has
        // not seen the rows yet.
        include: None,
    };
    Ok((params, limit))
}

/// Merges `sync.plan_done` into the tool's payload.
///
/// Serialized from the wire and NOT rebuilt field by field: this way
/// `counts`/`dest_trash`/`blockers`/`executable`'s spelling is EXACTLY the
/// protocol's without copying it by hand twice. `plan_hash` (of no use to
/// anyone outside the streams connection) and the wire's `task_id` — the
/// SAME one the bridge already set — are removed first.
///
/// **The merge does NOT OVERWRITE.** `serde_json::Map::extend` overwrites,
/// and extending the payload WITH the `Done` would mean a future wire field
/// named `complete` —or `truncated`, or `state`— would silently replace the
/// honesty flag this bridge computes. Latent today; a new wire key should
/// not be able to break it.
///
/// # Errors
/// If the `SyncPlanDone` fails to serialize (it cannot: a flat struct).
fn merge_plan_done(
    payload: &mut Value,
    done: &methods::SyncPlanDone,
    steps_total: u64,
) -> Result<(), String> {
    let done_json = serde_json::to_value(done)
        .map_err(|e| format!("tool sync_plan: could not encode plan_done: {e}"))?;
    let (Value::Object(mut obj), Some(target)) = (done_json, payload.as_object_mut()) else {
        return Ok(());
    };
    obj.remove("plan_hash");
    obj.remove("task_id");
    for (k, v) in obj {
        match target.entry(k) {
            serde_json::map::Entry::Vacant(e) => {
                e.insert(v);
            }
            serde_json::map::Entry::Occupied(e) => tracing::warn!(
                key = %e.key(),
                "sync.plan_done carries a key the bridge already sets: keeping the bridge's"
            ),
        }
    }
    target.insert("steps_total".to_owned(), json!(steps_total));
    Ok(())
}

/// Like [`map_backend_err`], but for `sync.plan`: the rejection an agent
/// reaches without doing anything odd is the per-connection cap on RETAINED
/// plans, and what it needs to be told is what to do about it.
///
/// Since #182 that rejection arrives with TAXONOMY
/// ([`Error::LIMIT_RETAINED_SYNC_PLANS`](norte_proto::Error::LIMIT_RETAINED_SYNC_PLANS)),
/// not as "internal error", so this function no longer guesses the cause: it
/// READS it. What is left is the advice, which the taxonomy does not carry
/// and the agent needs — above all "do not retry in a loop", because
/// retrying is what fills this cap.
///
/// Gone with the fix: the two constants copied from `daemon::server` (which
/// keeps them private) and the paragraph that named the "probable" cause
/// without being able to assert it.
fn map_plan_err(e: norte_proto::Error) -> String {
    match e {
        norte_proto::Error::LimitExceeded { ref limit }
            if limit == norte_proto::Error::LIMIT_RETAINED_SYNC_PLANS =>
        {
            format!(
                "sync.plan was refused: this connection is holding as many retained plans as \
                 the daemon allows. They expire on their own after ~{SYNC_PLAN_TTL_MIN_HINT} \
                 minutes. Do NOT retry in a loop: report the plans you already have and let \
                 the older ones expire."
            )
        }
        other => map_backend_err(other),
    }
}

/// The retained plan's TTL, in minutes ([`methods::SYNC_PLAN_TTL_MS`]): the
/// ONLY thing still worth naming, and it comes from the wire instead of
/// being copied.
const SYNC_PLAN_TTL_MIN_HINT: u64 = methods::SYNC_PLAN_TTL_MS / 60_000;

/// Translates `compare`'s and `sync_plan`'s optional `criteria` arg (array of
/// strings) into [`methods::CompareCriteria`]: absent or `null` = the wire's
/// default (size + date, no hash); present = EXACTLY the requested list,
/// never added to the default (a bare `["hash"]` turns on only `hash`).
///
/// An EMPTY list is an ERROR, and this is where this contract is **not** the
/// CLI's `--criteria`'s. `parse_compare_criteria` treats an empty list as
/// absent because clap does not distinguish "did not say it" from "said it
/// empty"; JSON does distinguish them, and treating them the same would cost
/// the following:
///
/// - in `compare`, no rung decides, so `norte_compare` gives
///   `same`/`presence`/`unknown` to EVERY pair present on both sides. With
///   `complete: true`. The agent reports two identical trees without having
///   compared anything.
/// - in `sync_plan`, worse: `Same` + `Unknown` with `on_unknown: copy`, this
///   tool's default, produces an `Overwrite` PER FILE. `criteria: []` would
///   turn "plan an update" into "rewrite the entire destination", with
///   `executable: true`.
///
/// An empty argument cannot be the shortest way to ask for that. It is
/// rejected with the same criterion as `mode`, which also has no neutral
/// value, and both schemas also carry `"minItems": 1` — which is advisory,
/// so the check lives here.
fn compare_criteria_arg(args: &Value) -> Result<methods::CompareCriteria, String> {
    let names = match args.get("criteria") {
        None | Some(Value::Null) => return Ok(methods::CompareCriteria::default()),
        Some(v) => v
            .as_array()
            .ok_or("arg criteria must be an array of strings")?,
    };
    let mut criteria = methods::CompareCriteria {
        size: false,
        mtime: false,
        hash: false,
    };
    for name in names {
        match name.as_str() {
            Some("size") => criteria.size = true,
            Some("mtime") => criteria.mtime = true,
            Some("hash") => criteria.hash = true,
            _ => return Err(format!("arg criteria: unknown criterion {name}")),
        }
    }
    if !(criteria.size || criteria.mtime || criteria.hash) {
        return Err(
            "arg criteria: name at least one of \"size\", \"mtime\", \"hash\" \
                    (an empty list runs no comparison at all and would report the two \
                    trees as equal); omit criteria for the wire default (size+mtime)"
                .to_owned(),
        );
    }
    Ok(criteria)
}

/// Default mtime tolerance, READ from the wire instead of copied: the
/// literal lives in `norte_proto`'s `default_mtime_tolerance_ms` (private)
/// and [`methods::SyncCompareOptions::default`] is its only public exit.
/// Copying the `2000` here means the day the wire changes it the bridge is
/// left with the old one.
fn default_mtime_tolerance_ms() -> u32 {
    methods::SyncCompareOptions::default().mtime_tolerance_ms
}

/// A stream tool's (`compare`, `sync_plan`) `limit`: absent = its default,
/// clamped to the hard ceiling, and `0` is an ERROR.
///
/// `limit: 0` is not "zero rows by the caller's choice", it is a nonsensical
/// argument (encoding-auditor, task 2 review): without this check it would
/// come out `truncated: true` with the empty list on the VERY FIRST element,
/// indistinguishable from a genuinely truncated tree. The JSON schema
/// already says `"minimum": 1`, but that is advisory — a real MCP client can
/// send it anyway.
fn stream_limit_arg(args: &Value, default: usize, max: usize) -> Result<usize, String> {
    let limit = opt_u64_arg(args, "limit")?
        .map(|l| usize::try_from(l).map_err(|_| format!("arg limit too large: {l}")))
        .transpose()?
        .unwrap_or(default)
        .min(max);
    if limit == 0 {
        return Err("arg limit must be >= 1".to_owned());
    }
    Ok(limit)
}

/// A plan's total STEPS per its counters: the sum of the classes
/// (`unknown_kind` included, which exists precisely so none is missing).
///
/// It is what [`Bridge::tool_sync_plan`] checks against the received steps.
/// Saturating, like [`methods::SyncCounts::add`]'s sums: an overflowed
/// counter is a weird number, a panic on the path of a half-million-step
/// plan is a dead tool. `irreversible` is NOT summed — it is orthogonal to
/// the classes, its rustdoc says so — nor is `unmeasured_steps`, which is a
/// subset of `copy`+`overwrite`.
fn plan_steps_total(counts: &methods::SyncCounts) -> u64 {
    counts
        .create_dir
        .saturating_add(counts.copy)
        .saturating_add(counts.overwrite)
        .saturating_add(counts.delete_tree)
        .saturating_add(counts.skip)
        .saturating_add(counts.unknown_kind)
}

/// Can it be asserted that this stream result is ALL there was?
///
/// Four conditions, and none is extra:
///
/// 1. it was not truncated (the caller hit the cap);
/// 2. the terminal state was seen (`None` = [`TASK_WAIT`] expired);
/// 3. that state is `Completed` — `Cancelled` and `Failed` are not a clean
///    ending, and returning an empty list without saying so reads as "no
///    differences";
/// 4. what was received matches what the daemon says it emitted. It is the
///    condition that was missing: batches can be lost while LEAVING the task
///    `Completed` (see [`Bridge::tool_compare`]'s rustdoc).
fn stream_is_complete(
    truncated: bool,
    state: Option<&norte_proto::TaskState>,
    got: usize,
    emitted: u64,
) -> bool {
    !truncated
        && matches!(state, Some(norte_proto::TaskState::Completed))
        && u64::try_from(got).is_ok_and(|got| got == emitted)
}

/// Terminal state name for a stream tool's payload. `None` (the deadline
/// expired without seeing it) is `"running"`: the daemon's task was still
/// alive when this tool stopped watching it.
fn state_label(state: Option<&norte_proto::TaskState>) -> &'static str {
    match state {
        Some(norte_proto::TaskState::Completed) => "completed",
        Some(norte_proto::TaskState::Cancelled) => "cancelled",
        Some(norte_proto::TaskState::Failed { .. }) => "failed",
        _ => "running",
    }
}

/// The reason for a `"failed"`, or `null`. Goes alongside [`state_label`]:
/// without it, a comparison that blew up and one that found no differences
/// are the same empty list.
fn state_error(state: Option<&norte_proto::TaskState>) -> Value {
    match state {
        Some(norte_proto::TaskState::Failed { error }) => json!(error.to_string()),
        _ => Value::Null,
    }
}

/// The bridge's `ClientInfo`, a single one for the session's TWO connections.
fn client_info() -> methods::ClientInfo {
    methods::ClientInfo {
        name: "norte-mcp".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    }
}

/// Cancels a stream tool's Task on DROP, unless it was disarmed.
///
/// The bridge already cancelled on truncation. The other reason was
/// missing, and it is the same fact: `notifications/cancelled` (and the
/// transport dying) drop the tool's future, and `TaskRef` has no `Drop` —
/// dropping the local `rx` only removes the client's route, while the
/// daemon's pump keeps sending batches over a live connection, gets `true`
/// back and never sees a `ReceiverGone`. The walk (and, with
/// `criteria: ["hash"]`, the ENTIRE reading of both trees) kept going to the
/// end for nobody. Cancelling because we stopped reading but not because the
/// agent stopped wanting it is not a coherent rule; and the gap was
/// exploitable within a legitimate scope: opening hash comparisons and
/// cancelling them right away returns the [`MAX_INFLIGHT_TOOLS`] slot
/// instantly and leaves the walk running, up to `MAX_LIVE_TASKS_AGENTS`.
///
/// Same pattern as `norte_core::backend::remote`'s `CancelOnAbandon`
/// (an armed drop-guard that disarms on the normal path).
struct CancelOnAbandon {
    /// `None` = disarmed (the terminal was seen: nothing to cancel).
    canceller: Option<norte_core::backend::TaskCanceller>,
}

impl CancelOnAbandon {
    fn new(canceller: norte_core::backend::TaskCanceller) -> Self {
        Self {
            canceller: Some(canceller),
        }
    }

    /// The normal path: the Task is already terminal.
    fn disarm(&mut self) {
        self.canceller = None;
    }
}

impl Drop for CancelOnAbandon {
    fn drop(&mut self) {
        if let Some(canceller) = self.canceller.take() {
            canceller.cancel();
        }
    }
}

/// Snapshot of a task as tool JSON (state + error by category).
fn task_json(t: &norte_proto::TaskProgress) -> Value {
    json!({
        "task_id": t.task_id.get(),
        // Same vocabulary as the stream tools: a model reading
        // `task_status` and `compare` does not learn two names for one state.
        "state": state_label(Some(&t.state)),
        "error": state_error(Some(&t.state)),
        "bytes_done": t.bytes_done,
        "bytes_total": t.bytes_total,
    })
}

/// OPTIONAL integer argument with a single criterion (sec MINOR-1 / enc H3):
/// absent = `None`, present with a bad type (float, string, negative) = a
/// tool error — never a silent degradation that confuses a looping agent
/// (an ignored `offset: 2.0` would look applied).
fn opt_u64_arg(args: &Value, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("arg {key} must be a non-negative integer, got {v}")),
    }
}

/// Extracts and parses a required `VPath` argument.
fn vpath_arg(args: &Value, key: &str) -> Result<VPath, String> {
    let s = args
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string arg: {key}"))?;
    VPath::parse(s).map_err(|e| format!("invalid VPath {s:?}: {e}"))
}

/// `tools/call`'s MCP Result: the payload goes as JSON text inside `content`.
fn tool_content(payload: &Value, is_error: bool) -> Value {
    let text = if let Value::String(s) = payload {
        s.clone()
    } else {
        payload.to_string()
    };
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

fn rpc_result(id: &Value, result: &Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

fn rpc_error(id: &Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

/// Description of the `path` argument for every tool that takes one. A
/// single time: it used to be copied in three places, and three copies of a
/// schema description diverge just like two copies of a handshake.
const PATH_DESC: &str =
    "VPath URL: file:///…, sftp://host/…, s3://bucket/…, or composed zip:file:///a.zip!/inside";

/// The bridge's ten tools: the eight v1 ones (ADR 0024, surface = what the
/// wire already offers) plus `compare` and `sync_plan` (spec 3 phase B), the
/// first ones to consume the streams arm instead of answering directly.
///
/// **One function per tool, and this list is only the order.** They used to
/// be eight inside a `json!([…])` plus two apart, with nothing saying which
/// of the two conventions the eleventh one debuted; and that `json!([…])`
/// forced an `unreachable!` to unpack the `Value::Array`, i.e. a panic path
/// in non-test code.
fn tool_defs() -> Value {
    Value::Array(vec![
        list_dir_tool_def(),
        stat_tool_def(),
        read_file_tool_def(),
        transfer_tool_def("copy"),
        transfer_tool_def("move"),
        delete_tool_def(),
        task_status_tool_def(),
        request_scope_tool_def(),
        compare_tool_def(),
        sync_plan_tool_def(),
    ])
}

fn list_dir_tool_def() -> Value {
    json!({
        "name": "list_dir",
        "description": "List a directory managed by norte. Returns entries (path/kind/size/mtime_ms) and next_cursor when paginated. size/mtime_ms may be null (lazy listing); use stat for a specific path.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": PATH_DESC},
                "limit": {"type": "integer", "minimum": 1, "description": "page size"},
                "cursor": {"type": "string", "description": "next_cursor from the previous page"}
            },
            "required": ["path"]
        }
    })
}

fn stat_tool_def() -> Value {
    json!({
        "name": "stat",
        "description": "Metadata of one node (kind/size/mtime_ms).",
        "inputSchema": {
            "type": "object",
            "properties": {"path": {"type": "string", "description": PATH_DESC}},
            "required": ["path"]
        }
    })
}

fn read_file_tool_def() -> Value {
    json!({
        "name": "read_file",
        "description": "Read a byte range of a file. Returns text (UTF-8; lossy if not) plus base64 of the exact bytes when the content is not valid UTF-8, and eof. If the byte range splits a multibyte character, text will contain U+FFFD at the edges and base64 carries the exact bytes: reassemble multi-chunk reads from base64, never by concatenating text.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": PATH_DESC},
                "offset": {"type": "integer", "minimum": 0},
                "len": {"type": "integer", "minimum": 1, "description": "max bytes (capped server-side)"}
            },
            "required": ["path"]
        }
    })
}

/// `copy` and `move`: the SAME schema and the same governance, so one
/// function with the name as a parameter instead of two copies drifting apart.
fn transfer_tool_def(name: &str) -> Value {
    let description = if name == "copy" {
        "Copy a file or directory (recursive). Runs as a cancellable task and this call waits for its outcome. Requires a granted scope; an `ask` policy suspends until a human approves."
    } else {
        "Move/rename a file or directory. Same governance as copy (scope + policy)."
    };
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": {
                "from": {"type": "string", "description": PATH_DESC},
                "to": {"type": "string", "description": "exact destination path (existing destination = conflict)"}
            },
            "required": ["from", "to"]
        }
    })
}

fn delete_tool_def() -> Value {
    json!({
        "name": "delete",
        "description": "Delete a file or directory (recursive). Default mode is trash (recoverable); permanent requires explicit mode and policy approval.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": PATH_DESC},
                "mode": {"type": "string", "enum": ["trash", "permanent"], "description": "default trash"}
            },
            "required": ["path"]
        }
    })
}

fn task_status_tool_def() -> Value {
    json!({
        "name": "task_status",
        "description": "Current state of a norte task by id (running/completed/failed/cancelled). Works for the task_id returned by compare and sync_plan too: both connections of this bridge are the same agent actor.",
        "inputSchema": {
            "type": "object",
            "properties": {"task_id": {"type": "integer", "minimum": 1}},
            "required": ["task_id"]
        }
    })
}

fn request_scope_tool_def() -> Value {
    json!({
        "name": "request_scope",
        "description": "Request access to path subtrees with specific operations (copy/move/delete/mkdir/create) for a TTL. The request stays PENDING until a human grants it (`norte policy grant <request_id>` or from the TUI); retry your operation after the grant.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "roots": {"type": "array", "items": {"type": "string"}, "description": "subtree roots (VPath URLs)"},
                "ops": {"type": "array", "items": {"type": "string", "enum": ["copy", "move", "delete", "mkdir", "create"]}},
                "ttl_ms": {"type": "integer", "minimum": 1, "description": "time-to-live in milliseconds (capped server-side at 24h)"}
            },
            "required": ["roots", "ops", "ttl_ms"]
        }
    })
}

/// `compare`'s definition (spec 3 phase B, task 2).
///
/// The arguments are called `left`/`right` and not `a`/`b` because the rows
/// answer in `left`/`right`: a model should not have to deduce which of the
/// two was `a`.
fn compare_tool_def() -> Value {
    json!({
        "name": "compare",
        "description": "Compare two trees (files and directories) and report what differs between them. Read-only: this mutates nothing. Each row carries WIRE values (verdict/criterion/confidence as the protocol spells them, e.g. \"only_left\", never a translated label) and both paths as their wire form. `complete` is the ONLY field to trust before saying anything about the two trees: it is true just when the run reached its end, was not truncated, and the rows received match the count the daemon says it emitted (`rows_total`). It is false when the run was truncated at `limit` (and then the comparison was CANCELLED), when the deadline expired (`timed_out`), when the task ended `cancelled` or `failed` (`state` says which, `error` says why), and when rows went missing in transit. A false `complete` means UNKNOWN — never \"these trees match\".",
        "inputSchema": {
            "type": "object",
            "properties": {
                "left": {"type": "string", "description": PATH_DESC},
                "right": {"type": "string", "description": PATH_DESC},
                "criteria": {"type": "array", "minItems": 1, "items": {"type": "string", "enum": ["size", "mtime", "hash"]}, "description": "which rungs to run; absent = size+mtime (the wire default). An EMPTY list is rejected: it would run no rung at all and call every pair equal. hash reads full file contents on both sides and requires content scope."},
                "max_depth": {"type": "integer", "minimum": 0, "description": "root counts as depth 0; absent = unlimited"},
                "mtime_tolerance_ms": {"type": "integer", "minimum": 0, "description": "absent = the wire default (2000 ms, FAT-safe)"},
                "limit": {"type": "integer", "minimum": 1, "description": "max rows before the comparison is cancelled and truncated:true; default 500"}
            },
            "required": ["left", "right"]
        }
    })
}

/// `sync_plan`'s definition (spec 3 phase B, task 3).
fn sync_plan_tool_def() -> Value {
    json!({
        "name": "sync_plan",
        "description": "Returns what a one-way synchronisation would do. It does NOT apply anything, and there is no tool that does: applying is a human action in their own client. The plan's hash is retained for THIS connection only, so it cannot be handed to a user or another tool — report what would change and let the human plan it again in their client. `task_id` IS usable: poll it with task_status. `complete` is false whenever the run did not reach a clean end for ANY reason (the `limit` was hit and the plan was cancelled and `truncated` is true; the deadline expired and `timed_out` is true; a human cancelled the underlying task; planning failed; or steps went missing in transit, which is what `steps_total` lets you see) — `truncated` only names one of those causes, so check `complete`, not `truncated`, before trusting anything. Whenever the plan did not close, `counts`/`dest_trash`/`blockers`/`blockers_total`/`executable`/`steps_total` are ABSENT from the result (never zero, never invented) — treat their absence exactly like a truncated `compare`: unknown, not clean. `mode: \"mirror\"` deletes from the destination what the source does not have; `dest_trash` says whether that (and any overwrite) is recoverable at all — \"restorable\" (undo can bring it back), \"opaque\" (a trash exists but norte cannot name what it buried) or \"absent\" (nothing comes back). `executable` is the only field that says whether a human could actually run this plan as-is; a non-empty `blockers` (or `blockers_total` bigger than the list) means they could not, whatever the steps look like. This tool takes NEITHER max_depth NOR mtime_tolerance_ms: it always plans over the whole tree with the wire's default tolerance, so a plan does not inherit the narrowing of a compare you ran first. Approved plans are retained server-side per connection with a cap and a TTL of several minutes; a run that fails outright (not `truncated`) rather than returning a `complete: false` result may mean that cap was hit — wait for older plans to expire rather than retrying sync_plan in a tight loop.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "source": {"type": "string", "description": "Where the bytes come from. VPath URL: file:///…, sftp://host/…, s3://bucket/…, or composed zip:file:///a.zip!/inside"},
                "dest": {"type": "string", "description": "Where they would go. VPath URL: file:///…, sftp://host/…, s3://bucket/…, or composed zip:file:///a.zip!/inside"},
                "mode": {"type": "string", "enum": ["update", "mirror"], "description": "REQUIRED, no default: \"update\" only copies/overwrites what differs; \"mirror\" does that plus deletes from dest what source does not have."},
                "criteria": {"type": "array", "minItems": 1, "items": {"type": "string", "enum": ["size", "mtime", "hash"]}, "description": "which rungs to run; absent = size+mtime (the wire default). An EMPTY list is rejected: with no rung the planner would call every pair equal-but-unknown and, under the default on_unknown:copy, plan an Overwrite for every single file. hash reads full file contents on both sides and requires content scope."},
                "on_unknown": {"type": "string", "enum": ["copy", "skip"], "description": "what to do with a row the provider could not compare with certainty; default copy."},
                "limit": {"type": "integer", "minimum": 1, "description": "max steps before the plan is cancelled and truncated:true; default 500"}
            },
            "required": ["source", "dest", "mode"]
        }
    })
}

/// Cap on one transport line (16 MiB, like the norte wire's
/// `MAX_FRAME_BYTES`): a "line" with no `\n` never accumulates memory
/// unboundedly (security-reviewer MINOR-2) — it is dropped and `-32700` is
/// answered.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// CONCURRENT tools/call in flight per transport (#67): a reasonable MCP
/// client carries 1-2; the cap cuts off a runaway client with a response
/// error, never accumulating tasks without bound.
pub const MAX_INFLIGHT_TOOLS: usize = 8;

/// An in-flight tool (#67 + #72): its local cancellation token and the cell
/// holding its mutating `fs.*`'s JSON-RPC id (to forward `rpc.cancel` to the
/// daemon).
#[derive(Clone)]
struct InflightTool {
    token: CancellationToken,
    daemon_id: DaemonIdCell,
}

/// Serves MCP over stdio until EOF (the agent closes the pipe when done).
/// One message per line (NDJSON, MCP's stdio transport; cap
/// [`MAX_LINE_BYTES`]). stdout is EXCLUSIVE to the transport: any
/// diagnostics go through tracing (the binary must point the subscriber at
/// stderr).
///
/// # Errors
/// stdio I/O or the initial connection/handshake with the daemon.
pub async fn serve_stdio(socket: &std::path::Path, session: &str) -> Result<(), BridgeError> {
    let bridge = Bridge::connect(socket, session).await?;
    tracing::info!(session, "MCP bridge connected to the daemon");
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let stdout = tokio::io::stdout();
    serve_transport(bridge, stdin, stdout).await
}

/// The bridge's transport over ANY read/write pair (#67): `tools/call`s are
/// dispatched to CONCURRENT tasks (cap [`MAX_INFLIGHT_TOOLS`]) and the
/// responses go out through a single channel toward the writer — never two
/// interleaved lines. A suspended tool (a policy ask, `wait_terminal` on a
/// long task) no longer holds up `ping` or `notifications/cancelled`.
/// WATCH OUT: concurrent IN THE BRIDGE — the daemon serves its connection in
/// SERIES, so two tools that touch it queue up there; what always stays
/// alive is what does not touch the daemon
/// (ping/initialize/tools\/list/cancelled). `notifications/cancelled {requestId}`
/// aborts the in-flight tool WITHOUT a response (MCP spec); the underlying
/// daemon Task stays alive and GOVERNED (journal + undo) — only the wait is
/// abandoned.
///
/// # Errors
/// Transport I/O.
///
/// # Panics
/// Never: the locks' `expect`s document the poisoning invariant (nothing
/// panics with them held).
pub async fn serve_transport<R, W>(
    bridge: Bridge,
    mut reader: R,
    writer: W,
) -> Result<(), BridgeError>
where
    R: tokio::io::AsyncBufRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let bridge = Arc::new(bridge);
    // Single output: inline and task responses compete for the channel, the
    // writer serializes whole lines.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<String>(64);
    let writer_task = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(out) = out_rx.recv().await {
            if writer.write_all(out.as_bytes()).await.is_err()
                || writer.write_all(b"\n").await.is_err()
                || writer.flush().await.is_err()
            {
                break;
            }
        }
    });
    // In-flight tools, by serialized id: `notifications/cancelled` cancels
    // its token; the task's guard removes the entry when it ends.
    let inflight: Arc<std::sync::Mutex<std::collections::HashMap<String, InflightTool>>> =
        Arc::default();

    let mut line: Vec<u8> = Vec::new();
    // `true` = the current line already exceeded the cap: it is drained up
    // to the `\n` without accumulating and `-32700` is answered on close.
    let mut overflow = false;
    let result: Result<(), BridgeError> = loop {
        // Manual fill_buf/consume: `read_until` would accumulate without a
        // cap. The read races against the WRITER dying (broken stdout): with
        // no output, no more tools with effects are dispatched (review m3).
        let (nl_at, used) = {
            let chunk = tokio::select! {
                r = reader.fill_buf() => match r {
                    Ok(c) => c,
                    // A read error ALSO goes through the common teardown
                    // (cancel in-flight, drain writer) — review B3.
                    Err(e) => break Err(e.into()),
                },
                () = out_tx.closed() => break Ok(()),
            };
            if chunk.is_empty() {
                break Ok(()); // EOF
            }
            let nl_at = chunk.iter().position(|&b| b == b'\n');
            let take = nl_at.unwrap_or(chunk.len());
            if !overflow {
                if line.len() + take > MAX_LINE_BYTES {
                    overflow = true;
                    line.clear();
                } else {
                    line.extend_from_slice(&chunk[..take]);
                }
            }
            (nl_at, nl_at.map_or(chunk.len(), |i| i + 1))
        };
        reader.consume(used);
        if nl_at.is_none() {
            continue;
        }
        // Complete line.
        if overflow {
            overflow = false;
            let _ = out_tx
                .send(rpc_error(&Value::Null, -32700, "line too long"))
                .await;
            continue;
        }
        let text = String::from_utf8_lossy(&line).into_owned();
        line.clear();
        if text.trim().is_empty() {
            continue;
        }
        dispatch_line(&bridge, &text, &out_tx, &inflight).await;
    };
    tracing::info!("end of transport (EOF/errors): bridge terminated");
    // Teardown COMMON to every path: in-flight tools are abandoned (the peer
    // will no longer read their responses) and the writer is drained.
    for (_, tool) in inflight.lock().expect("sound inflight lock").drain() {
        tool.token.cancel();
        // #72: if the tool had a mutating fs.* in flight, forward its
        // rpc.cancel — withdraws the suspended Ask instead of waiting out
        // the TTL.
        if let Some(&daemon_id) = tool.daemon_id.get() {
            bridge.cancel_daemon_request(daemon_id);
        }
    }
    drop(out_tx);
    let _ = writer_task.await;
    result
}

/// MCP correlation key: the serialized JSON `id` (number or string).
fn id_key(id: &Value) -> String {
    id.to_string()
}

/// Removes the `inflight` entry on ANY exit from the tool's task (response,
/// cancel or panic) — same RAII pattern as `PendingGuard`.
struct InflightGuard {
    key: String,
    map: Arc<std::sync::Mutex<std::collections::HashMap<String, InflightTool>>>,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.map
            .lock()
            .expect("sound inflight lock")
            .remove(&self.key);
    }
}

/// Classifies and dispatches ONE line (#67): cheap ones answer inline; a
/// `tools/call` goes to its own task with a cancellation token.
async fn dispatch_line(
    bridge: &Arc<Bridge>,
    text: &str,
    out_tx: &tokio::sync::mpsc::Sender<String>,
    inflight: &Arc<std::sync::Mutex<std::collections::HashMap<String, InflightTool>>>,
) {
    let msg: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            let _ = out_tx
                .send(rpc_error(
                    &Value::Null,
                    -32700,
                    &format!("parse error: {e}"),
                ))
                .await;
            return;
        }
    };
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let Some(id) = msg.get("id").cloned() else {
        // Notification: `cancelled` aborts the in-flight tool; the rest
        // (initialized…) is ignored without a response (JSON-RPC).
        if method == "notifications/cancelled"
            && let Some(req_id) = msg.pointer("/params/requestId")
            && let Some(tool) = inflight
                .lock()
                .expect("sound inflight lock")
                .remove(&id_key(req_id))
        {
            tool.token.cancel();
            // #72: if the tool had launched a mutating fs.* against the
            // daemon, forward an rpc.cancel for THAT request — withdraws its
            // suspended Ask instead of leaving it zombie until the TTL. An
            // id not yet set (a tool that never got to call the daemon) =
            // nothing to cancel.
            if let Some(&daemon_id) = tool.daemon_id.get() {
                bridge.cancel_daemon_request(daemon_id);
            }
        }
        return;
    };
    if method == "tools/call" {
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let token = CancellationToken::new();
        // Cell for the tool's mutating fs.*'s JSON-RPC id (#72): shared
        // between the tool's task (which sets it) and the cancelled handler
        // (which reads it to forward rpc.cancel).
        let daemon_id: DaemonIdCell = Arc::default();
        // The lock lives in its own scope with NO awaits (the future's
        // Send). Admission is by VACANT ENTRY: a repeated in-flight id does
        // NOT overwrite (overwriting would orphan the previous token and the
        // cap would be bypassable by reusing the same id — security-reviewer
        // A1).
        let admission = {
            let mut map = inflight.lock().expect("sound inflight lock");
            if map.len() >= MAX_INFLIGHT_TOOLS {
                Err("too many concurrent tool calls")
            } else {
                match map.entry(id_key(&id)) {
                    std::collections::hash_map::Entry::Occupied(_) => {
                        Err("duplicate request id already in flight")
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(InflightTool {
                            token: token.clone(),
                            daemon_id: Arc::clone(&daemon_id),
                        });
                        Ok(())
                    }
                }
            }
        };
        if let Err(msg) = admission {
            let _ = out_tx.send(rpc_error(&id, -32000, msg)).await;
            return;
        }
        let bridge = Arc::clone(bridge);
        let out_tx = out_tx.clone();
        let guard = InflightGuard {
            key: id_key(&id),
            map: Arc::clone(inflight),
        };
        tokio::spawn(async move {
            // The guard removes the entry on ANY exit — including a panic
            // from the tool (without it, 8 panics would exhaust the
            // transport forever; rust-reviewer M1).
            let _guard = guard;
            tokio::select! {
                biased;
                out = bridge.tools_call(&id, &params, &daemon_id) => {
                    let _ = out_tx.send(out).await;
                }
                // Cancelled: WITHOUT a response (MCP spec) — the wait is
                // abandoned; the daemon's Task keeps going, governed.
                () = token.cancelled() => {}
            }
        });
        return;
    }
    // The cheap ones (initialize/ping/tools/list/unknown) answer inline:
    // never blocks (does not touch the daemon).
    if let Some(out) = bridge.handle_line(text).await {
        let _ = out_tx.send(out).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BLOCKER: `criteria: []` is not "the default", it is turning off all
    /// THREE rungs. In `compare` it leaves `same/presence/unknown` on every
    /// pair present on both sides —two "identical" trees without comparing
    /// anything— and in `sync_plan`, with the `on_unknown: copy` that is its
    /// default, an `Overwrite` per file.
    #[test]
    fn empty_criteria_is_error_not_wire_default() {
        let err = compare_criteria_arg(&json!({"criteria": []})).expect_err("empty list");
        assert!(err.contains("criteria"), "{err}");
        // Absent IS the wire default (size + date, no hash).
        let d = compare_criteria_arg(&json!({})).expect("absent");
        assert_eq!(d, methods::CompareCriteria::default());
        assert!(d.size && d.mtime && !d.hash, "{d:?}");
        // And `null` too: it is "did not say it", not "said it empty".
        assert_eq!(
            compare_criteria_arg(&json!({"criteria": Value::Null})).expect("null"),
            methods::CompareCriteria::default()
        );
        // Present = exactly what was asked for, never added to the default.
        let solo_hash = compare_criteria_arg(&json!({"criteria": ["hash"]})).expect("hash");
        assert!(!solo_hash.size && !solo_hash.mtime && solo_hash.hash);
    }

    /// The two schemas also say so in the contract the model reads.
    #[test]
    fn schemas_forbid_an_empty_criteria_list() {
        for def in [compare_tool_def(), sync_plan_tool_def()] {
            assert_eq!(
                def["inputSchema"]["properties"]["criteria"]["minItems"],
                json!(1),
                "{def}"
            );
        }
    }

    /// BLOCKER: the terminal state does NOT prove all the rows are there.
    /// The `compare.rows` feed is routed with `OnFull::DropBatch`, so a
    /// dropped batch leaves the Task `Completed` and the channel cleanly
    /// closed.
    #[test]
    fn complete_requires_rows_to_match_the_emitted_ones() {
        let completed = norte_proto::TaskState::Completed;
        assert!(stream_is_complete(false, Some(&completed), 7, 7));
        assert!(
            !stream_is_complete(false, Some(&completed), 6, 7),
            "a row lost in transit with the task Completed is NOT complete"
        );
        assert!(
            !stream_is_complete(true, Some(&completed), 7, 7),
            "truncated"
        );
        assert!(!stream_is_complete(false, None, 7, 7), "no terminal seen");
        assert!(
            !stream_is_complete(false, Some(&norte_proto::TaskState::Cancelled), 7, 7),
            "cancelled"
        );
        assert!(
            !stream_is_complete(
                false,
                Some(&norte_proto::TaskState::Failed {
                    error: norte_proto::Error::Internal { panic: false }
                }),
                7,
                7
            ),
            "failed"
        );
    }

    /// A plan's step total is the sum of the CLASSES; `irreversible` is
    /// orthogonal and `unmeasured_steps` a subset, so neither is summed.
    #[test]
    fn the_total_steps_add_up_the_classes_and_only_the_classes() {
        let counts = methods::SyncCounts {
            create_dir: 1,
            copy: 2,
            overwrite: 3,
            delete_tree: 4,
            skip: 5,
            unknown_kind: 6,
            irreversible: 99,
            bytes: 12345,
            unmeasured_steps: 2,
        };
        assert_eq!(plan_steps_total(&counts), 21);
        assert_eq!(plan_steps_total(&methods::SyncCounts::default()), 0);
    }

    /// The deadline and the terminal state are counted separately in the
    /// payload: `None` is "was still alive when we stopped watching", not
    /// "ended".
    #[test]
    fn the_status_travels_with_a_name_and_the_failure_with_its_cause() {
        assert_eq!(state_label(None), "running");
        assert_eq!(
            state_label(Some(&norte_proto::TaskState::Completed)),
            "completed"
        );
        assert_eq!(
            state_label(Some(&norte_proto::TaskState::Cancelled)),
            "cancelled"
        );
        let failed = norte_proto::TaskState::Failed {
            error: norte_proto::Error::PermissionDenied,
        };
        assert_eq!(state_label(Some(&failed)), "failed");
        assert!(state_error(Some(&failed)).is_string(), "the reason travels");
        assert!(state_error(Some(&norte_proto::TaskState::Completed)).is_null());
    }

    /// `limit: 0` is not "zero rows on purpose": without the rejection it
    /// would come out `truncated: true` with an empty list, indistinguishable
    /// from a genuinely truncated tree.
    #[test]
    fn zero_limit_is_error_and_absent_is_default() {
        assert!(stream_limit_arg(&json!({"limit": 0}), 500, 5000).is_err());
        assert_eq!(
            stream_limit_arg(&json!({}), 500, 5000).expect("absent"),
            500
        );
        assert_eq!(
            stream_limit_arg(&json!({"limit": 9_000_000}), 500, 5000).expect("ceiling"),
            5000,
            "the hard ceiling overrides whatever the caller asks for"
        );
    }

    /// The default tolerance is READ from the wire, not copied.
    #[test]
    fn the_default_tolerance_is_the_wires() {
        assert_eq!(
            default_mtime_tolerance_ms(),
            methods::SyncCompareOptions::default().mtime_tolerance_ms
        );
    }

    /// The guard cancels on drop, and does NOT cancel if it was disarmed
    /// (the normal path: the terminal was seen).
    #[test]
    fn the_guard_cancels_on_drop_and_stays_quiet_if_disarmed() {
        let token = CancellationToken::new();
        drop(CancelOnAbandon::new(
            norte_core::backend::TaskCanceller::Embedded(token.clone()),
        ));
        assert!(token.is_cancelled(), "dropping the future cancels the walk");

        let token = CancellationToken::new();
        let mut guard =
            CancelOnAbandon::new(norte_core::backend::TaskCanceller::Embedded(token.clone()));
        guard.disarm();
        drop(guard);
        assert!(!token.is_cancelled(), "the normal path cancels nothing");
    }

    /// #182: a connection's 17th plan arrives with its TAXONOMY, and the text
    /// given to the agent tells it what to do — never "internal error",
    /// which is exactly the string that makes it retry, and retrying is what
    /// fills this cap.
    #[test]
    fn the_plan_error_names_the_cap_instead_of_saying_internal() {
        let text = map_plan_err(norte_proto::Error::LimitExceeded {
            limit: norte_proto::Error::LIMIT_RETAINED_SYNC_PLANS.to_owned(),
        });
        assert!(text.contains("retained plans"), "{text}");
        assert!(
            text.contains("Do NOT retry"),
            "the advice that matters: {text}"
        );
        assert!(
            !text.contains("internal error"),
            "not even naming it: it is THE string that causes a retry: {text}"
        );
        // And ANOTHER limit (a huge container) does not disguise itself as
        // the plan cap: each token says its own thing.
        let other = map_plan_err(norte_proto::Error::LimitExceeded {
            limit: norte_proto::Error::LIMIT_ENTRIES.to_owned(),
        });
        assert!(!other.contains("retained plans"), "{other}");
        // Everything else still comes out with the usual actionable text.
        let denied = map_plan_err(norte_proto::Error::PolicyDenied {
            rule: "r".to_owned(),
        });
        assert!(denied.contains("request_scope"), "{denied}");
    }

    /// The ten tools, each with its own function, and none named `sync_apply`.
    #[test]
    fn the_tools_catalog_does_not_have_two_conventions() {
        let Value::Array(defs) = tool_defs() else {
            panic!("tool_defs returns an array")
        };
        let names: Vec<&str> = defs
            .iter()
            .map(|d| d["name"].as_str().expect("name"))
            .collect();
        assert_eq!(
            names,
            [
                "list_dir",
                "stat",
                "read_file",
                "copy",
                "move",
                "delete",
                "task_status",
                "request_scope",
                "compare",
                "sync_plan"
            ]
        );
        // `path_desc` used to be copied three times; now it is a constant.
        for def in &defs {
            for prop in ["path", "from", "left", "right"] {
                let d = &def["inputSchema"]["properties"][prop]["description"];
                if let Some(text) = d.as_str() {
                    assert_eq!(text, PATH_DESC, "{def}");
                }
            }
        }
    }

    /// `compare` asks with the same names it answers with.
    #[test]
    fn compare_asks_with_the_same_names_it_answers_with() {
        let def = compare_tool_def();
        assert_eq!(def["inputSchema"]["required"], json!(["left", "right"]));
    }
}
