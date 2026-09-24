//! Protocol methods (spec §11): JSON-RPC method names and their params/result
//! types. M0 covers the `fs.*` family and the `task.progress` notification;
//! the rest of the families arrive with their milestones.
//!
//! Convention: each method has its own params struct and result struct —
//! adding an optional field is compatible; removing or renaming one requires
//! a [`PROTOCOL_VERSION`] bump.
//!
//! Typical flow (request → task → progress):
//!
//! ```
//! use norte_proto::methods::{FS_COPY, FsCopyParams, FsTaskResult};
//! use norte_proto::VPath;
//!
//! let params = FsCopyParams {
//!     from: VPath::parse("file:///src/a.txt").unwrap(),
//!     to: VPath::parse("file:///dst/a.txt").unwrap(),
//!     on_collision: Default::default(),
//!     symlinks: Default::default(),
//!     resume: Default::default(),
//!     verify: Default::default(),
//!     dest_anchor: None,
//!     queued: false,
//! };
//! let wire = serde_json::to_string(&params).unwrap();
//! let back: FsCopyParams = serde_json::from_str(&wire).unwrap();
//! assert_eq!(back, params);
//! assert_eq!(FS_COPY, "fs.copy");
//! // The result carries the TaskId; progress arrives via TASK_PROGRESS.
//! let result: FsTaskResult = serde_json::from_str(r#"{"task_id": 7}"#).unwrap();
//! assert_eq!(result.task_id.get(), 7);
//! ```

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    CollisionPolicy, DeleteMode, Entry, EntryKind, ResumePolicy, Segment, SymlinkPolicy, TaskId,
    VPath, VPathError, VerifyPolicy,
};

/// Protocol version (semver). The core supports N and N-1 (spec §11).
///
/// 0.9.0 (phase 8, ADR 0018): `READ_ONLY` capability + composite schemes
/// `zip+`/`tar+` with the `!` marker (archives as directories). Additive
/// over 0.8.x; the bump signals that the server understands composite paths.
///
/// 0.10.0 (M3-2): `TaskKind::Undo` + `TaskKind::Unknown` (forward-compat, like
/// `TaskState::Unknown`). Additive over 0.9.x; the bump signals that the
/// server knows how to emit session-undo Tasks.
///
/// 0.11.0 (M3-3b): policy engine over the protocol — `InitializeParams.
/// agent_session` (ties the connection to an agent session) + methods
/// `policy.request_scope`/`grant_scope`/`decide`/`pending` + notification
/// `policy.approval_required`. Additive over 0.10.x.
///
/// 0.12.0 (M3-4): `policy.undo_session` — a human undoes an agent's entire
/// session (strict LIFO, runs as a Task). Additive over 0.11.x.
///
/// 0.13.0 (M4-P3): `plugin.*` family — `plugin.list` (enumerates discovered
/// plugins + load errors, read-only), `plugin.set_approval` and
/// `plugin.set_enabled` (a human approves capabilities / enables a plugin;
/// User connections only). Additive over 0.12.x.
///
/// 0.14.0 (M4-P4): `plugin.run_command` — runs a command of an APPROVED and
/// ENABLED plugin (the plugin runs sandboxed; returns the command's string or
/// an error). Additive over 0.13.x.
///
/// 0.15.0 (M4-P5): `plugin.preview` — runs the first APPROVED and ENABLED
/// previewer plugin that handles the file's mimetype, over the bytes the
/// core reads; all `None` = no previewer applies. Additive over 0.14.x.
///
/// 0.16.0 (#71): `policy.undo_report` — report of an undo Task (what was
/// undone, what was skipped and why, where the LIFO got blocked): the human
/// undoing no longer gets a blind "done". Additive over 0.15.x.
///
/// 0.17.0 (#58): `Error::Corrupt` — broken container/format or over the
/// anti-bomb limits (previously forced into `Io{retryable:false}`, ADR 0018
/// D2): honest UX ("not a valid zip") and telemetry. Additive over
/// 0.16.x (an N-1 client degrades the kind to `Unknown`).
///
/// 0.18.0 (M4 live search): `fs.search` + `search.hits` + `TaskKind::Search`
/// — live search by name (glob/regex) and content (literal/regex,
/// multi-encoding) under a subtree, streamed as a cancelable Task. Additive
/// over 0.17.x (an N-1 client does not know `fs.search`/`search.hits` and
/// sees `TaskKind::Search` as `Unknown`, like the rest of the new kinds).
///
/// 0.19.0 (#72): `rpc.cancel { id }` notification (client→server) —
/// withdraws the in-flight request suspended in a policy Ask. Additive over
/// 0.18.x (an N-1 client/daemon ignores it; degrades to the zombie Ask until
/// its TTL, does not break).
///
/// 0.20.0 (#44): `connection.degraded` notification (server→client) — a
/// remote session was established with degraded security (FTP `tls="allow"`
/// → plaintext). Additive over 0.19.x (an N-1 client ignores it; log-only,
/// does not break).
///
/// 0.21.0 (#55, ADR 0028): `tar+gz` in `ARCHIVE_FORMATS`, longest-match
/// resolution, `scheme_archive_format` helper. Additive over 0.20.x — a
/// 0.20 peer sees `tar+gz+…` as Unsupported, without corruption.
///
/// 0.22.0 (#93): optional `skipped` field on [`FsListResult`] — total number
/// of CONTAINER entries omitted from the index (hostile names/limits, archive
/// providers). Additive over 0.21.x: absent when it does not apply (an N-1
/// client ignores it as an unknown field; without it, degrades to how it was
/// before, the counter used to live only in logs).
///
/// 0.23.0 (#95): [`Error::LimitExceeded`](crate::Error::LimitExceeded)
/// variant `{limit}` — exceeding a LOCAL anti-bomb limit no longer disguises
/// itself as `Corrupt` (a lie for a legitimately huge container). Additive
/// over 0.22.x: an N-1 client degrades it to `Error::Unknown` (generic
/// error, the same coarse UX as before).
///
/// 0.24.0 (#56, ADR 0018 A3): MULTI-LAYER archive addressing —
/// `zip+tar+file:///b.tar/!/i.zip/!/f` (right-to-left resolution: the
/// leftmost format is the outermost layer and cuts at its LAST marker; a
/// flat interior keeps the v1 rule of the first one).
/// `archive_compose` accepts an outer that is itself a well-formed archive
/// path; new `Error::LIMIT_NESTING` in the `LimitExceeded` vocabulary (a
/// layer limit, governed by the engine). Additive over 0.23.x: an N-1 peer
/// cleanly rejects nested paths (its `archive_split` gave `InvalidScheme` for
/// a composite interior → `InvalidPath` on the wire) — new addressing, it
/// never resignifies an old one: single-layer paths resolve identically.
///
/// 0.25.0 (M4, ADR 0034): INDEXED search. Methods `index.build` (a Task,
/// [`IndexBuildParams`]→[`IndexBuildResult`]) and `index.query`
/// ([`IndexQueryParams`]→[`IndexQueryResult`] with [`IndexHit`]), plus
/// [`TaskKind::Index`](crate::TaskKind::Index). Additive over 0.24.x: an N-1
/// peer does not know the methods (rejects them with `MethodNotFound`) and
/// degrades `TaskKind::Index` to `Unknown` via `serde(other)` — it never
/// resignifies anything old. A missing index (daemon without `norte-index`)
/// responds `Unsupported`.
///
/// 0.26.0 (P1): [`PluginInfo`] gains `description` (`Option<String>`,
/// cosmetic, absent = `None`) and `commands` (`Vec<`[`PluginCommandInfo`]`>`,
/// a catalogue of commands invocable via [`PLUGIN_RUN_COMMAND`]). Additive
/// over 0.25.x: an N-1 peer ignores both unknown fields when deserializing;
/// an N-1 peer building its own `PluginInfo` simply does not emit them and
/// this core falls back to their defaults (`None`/`vec![]`) — the window
/// moves to N=0.26.x/N-1=0.25.x.
///
/// 0.27.0 (G3, ADR 0037): three new methods for STRUCTURED plugin data, which
/// the HOST paints (never the plugin, which never gets direct painting
/// capability):
/// - [`PLUGIN_PREVIEW_STYLED`] — styled twin of [`PLUGIN_PREVIEW`]:
///   [`PluginPreviewStyledResult`] wraps (`flatten`, same all-or-nothing
///   pattern) a [`PluginPreviewStyled`] with `lines: Vec<Vec<`[`SpanWire`]`>>`.
/// - [`PLUGIN_DECORATE`] — git-status-like decorations per entry
///   ([`PluginDecorateResult`], POSITIONAL 1:1 with `params.paths`).
/// - [`PLUGIN_COLUMN_VALUES`] — values of a column contributed by a plugin
///   ([`PluginColumnValuesResult`], also positional 1:1).
///
/// The three are NEW methods (not flags on existing ones). The N/N-1 window
/// of [`version_compatible`] is NARROWER than "unknown method": a 0.27
/// client NEVER gets to call them against a 0.26 daemon, because
/// `initialize` already rejects that handshake with `VERSION_MISMATCH` (a
/// client from the future does not negotiate, see the doctest of
/// [`version_compatible`]) — the client never sees `MethodNotFound` in THAT
/// scenario, it sees the version failure before attempting anything.
/// `MethodNotFound` (taxonomy of unknown methods, ADR 0004) DOES apply
/// within the SAME 0.27 window: a 0.27 daemon that does not yet have the
/// handler wired (this bump is wire-only; T3/T4 wire it) responds
/// `MethodNotFound` to a 0.27 client, which falls back to the existing flat
/// surface ([`PLUGIN_PREVIEW`], no decorations, no columns) — same as any
/// client that decides NOT to call them after inspecting
/// `InitializeResult::protocol_version` and preferring not to risk it. Wire
/// limits (server ENFORCEs, client re-validates fail-closed to the flat
/// surface if violated): ≤10,000 lines, ≤256 spans/line (64 until 0.66.0: an
/// image paints one span per cell), span text ≤4 KiB, total payload
/// ≤4 MiB (the same runtime return limit already used by
/// [`PLUGIN_RUN_COMMAND`]/[`PLUGIN_PREVIEW`]), badge ≤8 chars AFTER masking.
/// `role: Option<String>` on [`SpanWire`]/[`DecorationWire`] is validated
/// HOST-SIDE against the closed `norte_theme::Role` set: an unknown name
/// degrades to `None` + warning, never to a hard error (the same lenient
/// treatment ADR 0020 gives a malformed theme). Additive over 0.26.x — the
/// window moves to N=0.27.x/N-1=0.26.x.
///
/// 0.28.0 (G3c, ADR 0037): closes the TWO debts G3 accumulated.
/// - [`PluginInfo`] gains `columns` (`Vec<`[`PluginColumnInfo`]`>`, additive —
///   default `vec![]`, same criterion as `commands` in 0.26.0): the
///   extension manager's columns UI can now DISCOVER which columns each
///   plugin contributes without guessing via `category == "columns"`.
/// - Two new methods that expose P2's `[config]` OVER THE WIRE (P2
///   deliberately left it host-only, deferred to this bump — see the
///   rustdoc of `norte_core::PluginRegistry::settings_of`):
///   [`PLUGIN_GET_CONFIG`] (schema + effective value, `keys: Vec<`[`PluginConfigKeyWire`]`>`)
///   and [`PLUGIN_SET_CONFIG`] (persists ONE value after validating it
///   against the SAME schema — never a parallel validation path; HUMAN
///   ONLY, same criterion as [`PLUGIN_SET_APPROVAL`]). `min`/`max` are
///   `Option<i64>` (`skip_serializing_if` when absent), `values` is
///   `Vec<String>` (empty for non-enum types, ALWAYS present — the same
///   "additive always present" criterion as `commands`), `description` is
///   PLUGIN text — NOT trustworthy (same treatment as `PluginCommandInfo::title`).
///   Additive over 0.27.x — the window moves to N=0.28.x/N-1=0.27.x.
///
/// 0.29.0 (#101): [`PluginPreview`] and [`PluginPreviewStyled`] gain `lossy:
/// bool` — the core flags when host-side text decoding (§6.2, #29) was
/// LOSSY (`had_errors`), so preview mode can flag decoding `�` the same way
/// the raw viewer already does. Additive over 0.28.x
/// (`#[serde(default)]` = an N-1 pair reads as `false`) — the window moves to
/// N=0.29.x/N-1=0.28.x.
///
/// 0.30.0 (columns block 1, ADR 0039): PROVIDER ATTRIBUTES, typed and
/// on-demand — FOUR additive fields spread across THREE surfaces (catalogue,
/// request ×2, entry) plus the vocabulary of the
/// [`attrs`](crate::attrs) module ([`AttrType`](crate::AttrType),
/// [`AttrHint`](crate::AttrHint), [`AttrInfo`](crate::AttrInfo),
/// [`AttrValue`](crate::AttrValue)). [`FsCapabilitiesResult`] gains
/// `attrs: AttrCatalog` (discovery: what THAT provider publishes, with type
/// and a presentation hint; on the wire, the usual array of `AttrInfo`);
/// [`FsListParams`] and [`FsStatParams`] gain
/// `attrs: Vec<String>` (the client asks ONLY for the ids it is going to
/// paint, nothing is delivered without asking); [`Entry`] gains
/// `attrs: BTreeMap<String, AttrValue>`. All four carry
/// `skip_serializing_if` over empty, so a 0.29 peer emits and receives
/// payloads BYTE-FOR-BYTE identical to before — additive in the strong
/// sense, not just in the "unknown field gets ignored" sense. The REQUEST
/// surfaces are `fs.list` and `fs.stat` and only those: neither the
/// [`SEARCH_HITS`] entries nor the [`INDEX_QUERY`] hits carry attributes in
/// 0.30.
///
/// `AttrValue` deserializes BY HAND (same route as
/// [`CapabilityFlags`](crate::CapabilityFlags): `#[serde(other)]` does not
/// exist for a variant WITH data) and degrades to `AttrValue::Unknown` for
/// EVERY malformed value — unknown tag (a peer from the future, ADR 0004
/// applied at cell granularity), AMBIGUOUS object with two or more known
/// tags (a JSON object's keys are not ordered, RFC 8259 §4, so "first one
/// wins" would depend on a relay's whim), payload of the wrong JSON type,
/// `null`, non-object, undecodable base64, and text or bytes above the
/// limit. None of these is a hard error: it costs ONE cell, never the entry
/// nor the page.
///
/// The two RECEIVING fields filter on decode and also never err. `Entry.attrs`:
/// a malformed id is DISCARDED, the map is bounded at
/// [`ATTRS_MAX_REQUEST`](crate::ATTRS_MAX_REQUEST), keeping the smallest ids
/// in byte order, repeated key last-wins.
/// `FsCapabilitiesResult.attrs`: malformed id discarded, REPEATED id
/// first-wins (it is an ORDERED list the provider ranks, unlike an unordered
/// JSON object), long label TRUNCATED to
/// [`ATTR_LABEL_MAX`](crate::ATTR_LABEL_MAX) on a char boundary (the id is
/// what a client acts on: losing the attribute over something cosmetic would
/// be the bad change), catalogue bounded at
/// [`ATTRS_MAX_ADVERTISED`](crate::ATTRS_MAX_ADVERTISED) keeping the FIRST
/// ones off the wire, and scanning bounded at
/// [`ATTRS_MAX_CATALOG_SCAN`](crate::ATTRS_MAX_CATALOG_SCAN) (the rest is
/// drained without materializing). The catalogue is also a type, not a call:
/// [`AttrCatalog`](crate::AttrCatalog) has a private vector and a single
/// constructor that sanitizes, so the EMBEDDED path (TUI/CLI by default,
/// which does not cross deserialization) is covered the same as the wire
/// one.
///
/// The two REQUEST fields, on the other hand, do NOT filter on purpose: they
/// carry data that THIS peer SENDS, so a malformed id survives the decode and
/// becomes the daemon's `-32602` — wired up by block 2 — instead of being
/// whitewashed to "asked for nothing", which would hide the caller's bug and
/// make that validation untestable. With one exception that is NOT
/// validation but a MEMORY bound: the first
/// [`ATTRS_MAX_REQUEST`](crate::ATTRS_MAX_REQUEST) `+ 1` elements are kept
/// and the rest is drained without materializing, because a 16 MiB frame of
/// tiny ids would reserve ~15× its size in `String` headers before any
/// daemon check could run. So a malformed id ALWAYS survives the decode, but
/// from element 17 onward the id no longer arrives at all: what survives is
/// the WITNESS that it was passed (`attrs.len() > ATTRS_MAX_REQUEST`), which
/// is what the daemon needs to reject instead of trimming the violation down
/// until it becomes legal.
///
/// This bump is WIRE-ONLY: no provider advertises attributes yet and the
/// daemon ignores the requested ids, which is honest because ABSENCE is
/// already a valid response under the contract (asking for an id the
/// provider does not offer was never an error: it comes back absent). The
/// window moves to N=0.30.x/N-1=0.29.x, and the direction that has to hold
/// is a 0.29 client against a 0.30 daemon: it does not send `attrs`, does
/// not receive `attrs`, nothing changes for it. The reverse is not a
/// question about attributes — [`version_compatible`] flatly rejects a
/// client from the FUTURE with `VERSION_MISMATCH`, before looking at any
/// field.
/// 0.31.0 (#104): new method `fs.mkdir` (additive — [`FsMkdirParams`] →
/// [`FsTaskResult`], the same Task shape as copy/move/delete) and variant
/// `TaskKind::Mkdir`. Window N=0.31.x / N-1=0.30.x: a 0.30 client never calls
/// the new method and degrades the new kind to `TaskKind::Unknown` via its
/// `serde(other)` (present since 0.10) — nothing to gate on emission.
/// 0.32.0 (M4-IA, ADR 0031): new method `ai.rename_plan` (additive —
/// [`AiRenamePlanParams`] → [`AiRenamePlanResult`], a direct response
/// cancelable with `rpc.cancel`). Window N=0.32.x / N-1=0.31.x: a 0.31 client
/// never calls the new method — nothing to gate on emission.
/// 0.33.0 (M4-IA-2, ADR 0031 A3): new methods `index.embed` (an embeddings
/// Task — [`IndexEmbedParams`] → [`FsTaskResult`]) and
/// `index.search_semantic` (a direct request cancelable with `rpc.cancel` —
/// [`IndexSearchSemanticParams`] → [`IndexSearchSemanticResult`]) plus the
/// `TaskKind::Embed` variant. Window N=0.33.x / N-1=0.32.x: a 0.32 client
/// never calls the new methods and degrades the new kind to
/// `TaskKind::Unknown` via its `serde(other)` — nothing to gate on emission.
/// 0.34.0 (H3e, ADR 0040): PLUGIN help over the wire — that ADR's corpus
/// reaching the protocol. [`PluginInfo`] gains
/// `has_help: bool` (cheap discovery, `skip_serializing_if` over `false` —
/// a plugin without help produces the SAME payload as in 0.33) and the
/// method [`PLUGIN_HELP`] ([`PluginHelpParams`] → [`PluginHelpResult`])
/// appears, delivering the already-bounded, already-decoded `help.md` plus
/// the `truncated`/`lossy` flags the receiver cannot infer. Window
/// N=0.34.x / N-1=0.33.x, and the direction that has to hold is a 0.33
/// client against a 0.34 daemon: it ignores the unknown field, does not emit
/// `has_help` (default `false` here) and never calls the new method —
/// nothing to gate on emission. The reverse is NOT a question about help:
/// [`version_compatible`] flatly rejects a client from the FUTURE with
/// `VERSION_MISMATCH` in `initialize`, before dispatching any method (the
/// same reasoning the 0.30.0 bump wrote above).
///
/// 0.35.0 (#120): [`PluginColumnValuesParams`] gains `plugin_id: Option<String>`.
/// `column_id` does not identify the plugin, and the host resolved
/// first-match, so two consented plugins declaring the same bare id made
/// `plugin:a/status` silently paint `b`'s values. The frontend always knew
/// which one it was; what was missing was a place to say so. Window
/// N=0.35.x / N-1=0.34.x in the direction that matters: a 0.34 client does
/// not emit the field, the host falls back to the old path and the result
/// is the same it had — including its ambiguity, which is exactly what an
/// old client already swallowed. `skip_serializing_if` over `None` leaves
/// the payload of a request without a plugin byte-for-byte identical to
/// 0.34's. The reverse is cut off by [`version_compatible`] in the
/// handshake, as always.
///
/// 0.36.0 (batch rename, ADR 0042): a batch of renames inside ONE directory
/// becomes ONE transaction. [`FS_RENAME_BATCH_PLAN`]
/// ([`FsRenameBatchPlanParams`] → [`FsRenameBatchPlanResult`], a DIRECT
/// response) and [`FS_RENAME_BATCH`] ([`FsRenameBatchParams`] → the existing
/// [`FsTaskResult`]) appear, plus [`TaskKind::RenameBatch`](crate::TaskKind)
/// and the categories [`Error::PlanStale`](crate::Error::PlanStale) and
/// [`Error::PlanNotExecutable`](crate::Error::PlanNotExecutable). The two
/// methods are tied together by `plan_hash`: the client sends INTENT
/// (`pairs`), never the order, and execution returns the hash of the plan
/// the human approved so the core can re-plan and compare — so a client,
/// which may be an AGENT, cannot slip in an order nobody reviewed. Names
/// travel as [`Segment`] (percent-encoded, hard rule 1), not as `String`: a
/// batch of renames is exactly where a non-UTF8 name has to survive byte for
/// byte. The CLOSED vocabulary of [`RenameCollisionKind`] is FOUR verdicts:
/// `internal`, `external`, `absent_source` and `ambiguous_source` — the last
/// one for the directory with twins, where the requested source folds onto
/// two entries and the core refuses to choose.
/// [`FS_RENAME_BATCH_REPORT`] also appears
/// ([`FsRenameBatchReportParams`] → [`FsRenameBatchReportResult`]), a twin of
/// [`POLICY_UNDO_REPORT`] (#71): a batch whose rollback got stuck leaves the
/// directory half-renamed, and that does not fit in a Task's `Failed` — it
/// needs to be able to NAME the file that ended up somewhere it should not
/// have. For the same reason, [`PolicyUndoReportResult`] gains `batch_stuck`
/// and `compensations_lost`: session undo could already undo a batch, and
/// until now the remote human only saw `blocked` — which says "I stopped,
/// the tree is consistent", exactly the opposite of what had happened. The
/// two fields are ADDITIVE, but not in the same way: `batch_stuck` is
/// omitted when there is none, while `compensations_lost` is optional on the
/// WAY IN (`serde(default)`) and mandatory on the WAY OUT — it always
/// travels, even at zero, like the report's other three counters. That
/// breaks the byte-for-byte identity of the clean payload of a type that has
/// existed since 0.16.0, and it is accepted knowingly: a counter that gets
/// omitted at zero is a counter whose absence is ambiguous, and this report
/// is the only place that counts this.
/// This bump's declared LIMITS are also contract:
/// [`FS_RENAME_BATCH_MAX_PAIRS`] (rejects, does not trim) and
/// [`PLAN_HASH_LEN`], which the [`PlanHash`] newtype enforces on
/// deserialization — a hash of another shape is a params error and never
/// `plan_stale`.
/// Window N=0.36.x / N-1=0.35.x: a 0.35 client does not know the new methods
/// and does not call them, and degrades the new `TaskKind` to
/// `TaskKind::Unknown` via its `serde(other)`; the two new error categories
/// fall into its `Error::Unknown` (same pattern as `CursorExpired` in
/// 0.8.0) — nothing to gate on emission, because they only appear answering
/// methods that client never invokes. The new fields of
/// `PolicyUndoReportResult` are IGNORED (serde is not
/// `deny_unknown_fields`), which is exactly what it did before they
/// existed: it loses the notice, not the report. The two notices do not
/// carry the same weight, though. An undo with `batch_stuck` FAILS the Task,
/// so a 0.35 client sees `Failed` and knows something happened, even if not
/// what. `compensations_lost`, on the other hand, does not change the
/// outcome: for that client the undo says `Completed` and the session will
/// block on the next attempt without anything having warned it — which is
/// exactly why the field describes itself as "the only signal of that". The
/// reverse is cut off by [`version_compatible`] in the handshake.
///
/// Whoever receives `MethodNotFound` for a `plugin.help` must treat it as
/// "this plugin has no page", never as a failure. The reason is that help is
/// COSMETIC: if the peer does not implement the method, there is no page to
/// paint and nothing is broken. It is not that an N-1 daemon answers that —
/// it never receives the call, because the handshake already rejected it.
///
/// 0.37.0 (#131, design `2026-08-10-volumes-design.md`): new method
/// [`HOST_VOLUMES`] ([`HostVolumesParams`] → [`HostVolumesResult`]), the
/// enumeration of the HOST's volumes — not a provider's, and that is why it
/// lives outside the `fs.*` family (design §A). [`Volume::mount`] is a
/// [`VPath`], never a `String` (hard rule 1: a non-UTF-8 mount point has to
/// survive the wire byte for byte); [`VolumeKind`] gains `#[serde(other)]`
/// over `Unknown`, the same shape as [`EntryKind::Other`] and
/// `TaskKind::Unknown`, so a volume type added in N+1 degrades instead of
/// breaking an old client. The method answers ONLY a `User` connection: the
/// mount table names the human's disks, servers and removable media, and an
/// agent under scope needs none of it (design §C) — the gate lives in
/// `norte-core::daemon` and responds `Error::PolicyDenied` with the closed
/// vocabulary of `DenyReason::rule_id()`, never the concrete rule. Window
/// N=0.37.x / N-1=0.36.x: a 0.36 client never calls the new method —
/// nothing to gate on emission.
///
/// 0.38.0 (task V3.5 of the volumes plan, an encoding-auditor finding
/// deferred by V3): [`Volume::label`] goes from `Option<String>` to
/// `Option<Vec<u8>>` (base64 on the wire, `label_wire` module — the same
/// shape as [`crate::attrs::AttrValue::Bytes`], not a third invention). A
/// `String` either lied or refused in the face of a non-UTF8 ext4/vfat label
/// (hard rule 1); it was LATENT because today's Linux never populates the
/// field, but V4 (macOS/Windows) will, and fixing it later would have broken
/// the wire twice. INCOMPATIBLE field-shape CHANGE inside a method that only
/// ever existed on this branch, unpublished (0.37.0 was never tagged or
/// released) — treated the same as any bump: the window shifts, it does not
/// widen. Window N=0.38.x / N-1=0.37.x.
///
/// 0.39.0 (directory comparison, design
/// `2026-08-11-directory-comparison-design.md`, ADR 0048): new method
/// [`FS_COMPARE`] ([`FsCompareParams`] → the EXISTING [`FsTaskResult`]) and
/// new notification [`COMPARE_ROWS`] ([`CompareRowsBatch`]), plus the
/// vocabulary the rows speak: [`CompareRow`], [`CompareVerdict`],
/// [`CompareCriterion`], [`CompareConfidence`], [`CompareReason`], [`Side`],
/// [`CompareCriteria`] and the limits [`COMPARE_ROWS_MAX_BATCH`] and
/// [`COMPARE_MAX_DIR_ENTRIES`]. The method travels with its own Task class,
/// [`TaskKind::Compare`](crate::TaskKind::Compare) — in THIS bump and not the
/// next one, because the `task_id` of a batch of rows correlates with a Task
/// the client has to be able to classify; a 0.38 client degrades it to
/// `TaskKind::Unknown` via its `serde(other)`. PURE ADDITIVE bump: no
/// existing type changes shape — none even gains fields, unlike what
/// `PolicyUndoReportResult` did in 0.36.0 — so the payload of any earlier
/// method stays byte-for-byte the one from 0.38.0.
///
/// What is NEW about the vocabulary, and the reason for the ADR, is that a
/// comparison DECLARES what its criterion has earned: each row carries the
/// rung that decided it and the confidence that rung deserves, and
/// [`CompareConfidence::Unknown`] — "the provider cannot say" — is an
/// ANSWER, not an error. That is why, and only there, the `#[serde(other)]`
/// fallback is not called `Unknown` but
/// [`CompareConfidence::Unrecognised`]: sharing the name would turn an
/// honest answer into a protocol mismatch.
///
/// Window N=0.39.x / N-1=0.38.x: a 0.38 client does not know the new method
/// and does not call it, so it never receives a row — nothing to gate on
/// emission. The reverse is cut off by [`version_compatible`] in the
/// handshake, as always.
///
/// 0.40.0 (directory synchronization, design
/// `2026-08-11-directory-sync-design.md`, ADR 0049): spec 2 of the same
/// roadmap item as 0.39.0. New methods [`SYNC_PLAN`] ([`SyncPlanParams`] →
/// the EXISTING [`FsTaskResult`]), [`SYNC_APPLY`] ([`SyncApplyParams`] → the
/// same) and [`SYNC_REPORT`] ([`SyncReportParams`] → [`SyncReportResult`]);
/// new notifications [`SYNC_STEPS`] ([`SyncStepsBatch`]) and
/// [`SYNC_PLAN_DONE`] ([`SyncPlanDone`]); the vocabulary the steps speak
/// ([`SyncStep`], [`SyncStepKind`], [`StepReversal`], [`SyncReason`],
/// [`SyncBlocker`], [`SyncBlockerKind`], [`SyncCounts`], [`SyncFailure`],
/// [`SyncFailureCause`], [`SyncMode`], [`OnUnknown`], [`RelPath`],
/// [`SyncCompareOptions`], [`DescendSide`], [`DestTrash`]) and the limits
/// [`SYNC_STEPS_MAX_BATCH`], [`SYNC_PLAN_TTL_MS`],
/// [`SYNC_MAX_BLOCKERS_REPORTED`] and [`SYNC_MAX_INCLUDE`]. The methods
/// travel with their two Task classes,
/// [`TaskKind::SyncPlan`](crate::TaskKind::SyncPlan) and
/// [`TaskKind::Sync`](crate::TaskKind::Sync), and a new error category,
/// [`Error::OverlappingRoots`](crate::Error::OverlappingRoots).
///
/// ADDITIVE bump. **One existing type does gain a field**, and it is the only
/// one: [`FsCompareParams`] gains [`FsCompareParams::descend_orphans`],
/// optional and omitted when absent. The comparison engine is the same one a
/// plan uses underneath, and the planner needs to descend the source's
/// orphan; duplicating the method to avoid touching its params would have
/// left two comparisons that diverge. It is compatible in the two directions
/// that matter: a 0.39 client's payload does not change by a single byte —
/// the field is omitted when it is `None` — and its absence means exactly
/// 0.39.0's behavior. Everything else in the bump is a new type, or a new
/// variant of an enum that already degraded (see the next paragraph), so the
/// payload of any other method stays byte-for-byte 0.39.0's. What this bump
/// does NOT do is restructure a published type — a `flatten`, a field that
/// changes type or name —: adding an optional, omitted key is not that.
///
/// What is NEW about the vocabulary is that a plan DECLARES what it can
/// undo: [`StepReversal`] travels per step and BEFORE approval, so the human
/// sees how many steps are irreversible while they can still say no, instead
/// of reading it in the report — together with [`SyncPlanDone::dest_trash`],
/// without which that column is not enough (see [`DestTrash`]). And the
/// `#[serde(other)]` asymmetry is deliberate: the vocabulary that goes
/// daemon→client carries it, like ADR 0048's; [`SyncMode`] and
/// [`OnUnknown`], which go client→daemon, do NOT — accepting an unknown mode
/// by default is accepting deletion by default.
///
/// **Within the branch itself, 0.40.0 moved once**: [`SyncPlanDone`] gained
/// [`SyncPlanDone::dest_trash`], mandatory and without a default (task 12 of
/// the sync plan). Since the version number did not change — 0.40.0 has not
/// been released — [`version_compatible`] does not distinguish a binary from
/// before this from one from after: a new client against an old daemon
/// cannot decode the closing message and is left waiting for a plan that
/// never closes. Two 0.40.0 binaries from different commits on this branch
/// are NOT interchangeable; it is the same criterion 0.38.0 wrote down for
/// [`Volume::label`], noted here so nobody diagnoses it as a hang.
///
/// Window N=0.40.x / N-1=0.39.x: a 0.39 client does not know the new methods
/// and does not call them, so it never receives a step or a `plan_done` —
/// nothing to gate on emission, except for the two Task classes: those DO
/// reach it without having called anything, because [`TASK_PROGRESS`] is
/// broadcast to every human connection and `task.list` returns them on
/// resync. Its `serde(other)` degrades them to `TaskKind::Unknown` — that is
/// this bump's only real N/N-1 surface, and it has a golden in
/// `task_progress.json`. A 0.39 client also does not send
/// `descend_orphans` (it does not know it) and its absence IS 0.39.0's
/// behavior, so [`FsCompareParams`]'s new field adds no surface at all in
/// that direction. The reverse — a 0.40 client sending the field to a 0.39
/// daemon, which serde would silently ignore — is cut off by
/// [`version_compatible`] in the handshake: in 0.x a client with a minor
/// GREATER than the server's does not negotiate.
///
/// 0.41.0 (#178): ONE new error category,
/// [`Error::JournalUnavailable`](crate::Error::JournalUnavailable) — this
/// session's journal cannot be opened, so the mutation refuses and nothing
/// is touched. No method, no notification, no field: the smallest bump there
/// is.
///
/// **Nobody emits it over the wire today, and the bump is paid anyway.** A
/// daemon with an unreadable journal never gets to start, so the only
/// possible emitter is the EMBEDDED transport, which talks in-process. The
/// bump costs interop — a 0.41 frontend stops negotiating with a 0.40
/// daemon — and does not buy a single byte of new conversation. It is paid
/// because the taxonomy gets PUBLISHED
/// (`docs/schema/proto.schema.json` ships with every release, #13): a third
/// party writing a client against that file has to be able to see the
/// category its `match` will receive the day a daemon can lose its journal
/// live. A category that exists in the type and not in the schema is the
/// kind of divergence ADR 0038 freezes the schema to avoid having.
///
/// **No fields, and with a place already reserved for the detail.** The
/// file path and the raw `SQLite` text are local to the process emitting it
/// — and the second one is partly shaped by whoever can write `journal.db`
/// — so they do not cross the boundary: they travel over the in-process
/// channel `norte_core::embedded::NoJournal` (which this crate cannot link
/// against: it is its consumer, not its dependency), sanitized. If some day
/// a daemon needs to name the file, the place already exists and does NOT
/// call for a bump: the `RpcError`'s `message` carries the `Display` (see
/// `impl From<Error> for RpcError`), which is where ADR 0004 puts the
/// readable detail. Adding a field to the variant later would also be
/// additive on the wire — serde ignores extra keys when decoding a unit
/// variant with an internal tag — and would only break the Rust API.
///
/// **No ADR, and why.** The product decision — an unreadable journal
/// REFUSES, and there is no `--no-journal` to skip it — is made by #178 and
/// lives in the rustdoc of the `norte_core::embedded` module, which is where
/// someone would go looking for it. What reaches the wire is one more
/// category in an enum that already degrades; it does not change the shape
/// of any message, the negotiation, or the trust model between endpoints.
/// Bumps of that size (0.29.0, 0.31.0, 0.35.0) did not carry an ADR either.
///
/// Window N=0.41.x / N-1=0.40.x: a 0.40 client receiving this category
/// degrades it to `Error::Unknown` via its `#[serde(other)]`, the mechanism
/// this enum has carried since M0 and that has its own test. In practice it
/// does not receive it: only the embedded transport emits it, and the
/// embedded transport has no wire.
///
/// 0.45.0 (#153, #145, #164, ADR 0054): a provider stops answering only
/// about itself. Two new capability flags —
/// [`CapabilityFlags::FULL_FOLD`](crate::CapabilityFlags::FULL_FOLD) (this
/// location folds by EXPANDING: an ext4/f2fs under `+F`) and
/// [`CapabilityFlags::CONFINED_WRITES`](crate::CapabilityFlags::CONFINED_WRITES)
/// (a write under this location can be confined under the root the caller
/// names) —, a conflict subtype ([`ConflictKind::EscapesRoot`](crate::ConflictKind::EscapesRoot))
/// and a pairing transform ([`PairTransform::FullFold`]).
///
/// The first three degrade on their own in a 0.44 client (unknown name gets
/// ignored, ADR 0004; subtype falls into `Unknown`, ADR 0005). The fourth is
/// what decided this had to be a NEW variant and not an existing value: two
/// names that only pair by expanding **do not name the same text**, and
/// folding them into `CaseFold` would have made
/// [`PairTransform::names_one_text`] answer `true` about a pair that may be
/// two files — with ADR 0053's gate in `norte-sync` overwriting behind it.
///
/// `fs.capabilities` does not change shape, but it does change MEANING: it
/// always took a path and now it answers for it.
///
/// 0.42.0 (#170, #152, #195): THREE fields, on three types that already
/// existed, and **a single bump**. All three are the same class of gap — a
/// message read without the context that produced it, missing the data that
/// context had at hand — so they carry one set of goldens, one schema
/// regeneration and one `protocol-guardian` review. Bumping three times for
/// three fields would have cost three N/N-1 windows for the same work.
///
/// * [`SyncReportResult::dest_trash`] (#170), mandatory, with the same value
///   that already traveled in [`SyncPlanDone::dest_trash`]. A client that
///   reconnected, that was not the one who planned, or that dropped the
///   `sync.plan_done` could read what got copied, overwritten and deleted,
///   and could not know whether any of it comes back.
/// * [`SyncFailure::kind`] (#195), mandatory, the same class the step
///   carried in the plan. Without it, which root a failure's `rel` hangs
///   from was DEDUCED from the presence of `dest_rel`, and that deduction is
///   only correct as long as the core never emits a `DeleteTree` with
///   `dest_rel` — an invariant the wire did not state and that a
///   `norte-sync` test upheld.
/// * [`CompareRow::paired_under`] (#152), OPTIONAL and omitted when absent.
///   Marks the pair whose two names are not the same bytes and says under
///   which transform it paired ([`PairTransform`]), which is the only thing
///   that separates a legitimate NFC/NFD pair from two distinct files joined
///   by an NFC singleton decomposition.
///
/// ADDITIVE bump, with the same asymmetry as 0.40.0: **two existing types
/// gain a MANDATORY field** and one gains an optional one. The two mandatory
/// ones go in the daemon→client direction and neither affects what a client
/// SENDS, so the payload of every request stays byte-for-byte 0.41.0's; the
/// optional one also leaves an ordinary comparison's payload intact, because
/// the key is omitted when there is no transform to name. No type is
/// restructured, no field changes name or type, and there is no new method
/// or notification.
///
/// **What this bump does NOT do is change pairing semantics.** The
/// `norte-compare` key still folds and normalizes exactly the same way, and
/// the same pairs still pair; what changes is that the row now SAYS so.
/// Changing who pairs with whom would need an ADR — and deciding what a sync
/// plan does with a [`PairTransform::NormalizationSingleton`] is that other
/// decision, which this bump deliberately leaves half-made: first the data,
/// then the policy.
///
/// Window N=0.42.x / N-1=0.41.x: a 0.41 client ignores the extra
/// `paired_under`, `kind` and `dest_trash` — serde discards keys it does not
/// know — so it keeps reading reports and rows without breaking; what it
/// cannot do is make use of them, which is exactly the debt this bump closes
/// for the next one. The reverse — a 0.41 daemon sending a report WITHOUT
/// `dest_trash` to a 0.42 client, which would fail to deserialize — does not
/// happen: [`version_compatible`] does not negotiate a client with a minor
/// GREATER than the server's.
///
/// **0.46.0** (roadmap item 10): [`DAEMON_GOING_AWAY`] and
/// [`DaemonShutdownParams::mode`]. Additive: `mode` is not serialized when it
/// is [`ShutdownMode::Stop`], so an ordinary stop comes out byte for byte
/// like 0.45's, and an unknown notification is ignored (ADR 0004). The
/// window still SHIFTS, and this is the clearest example of why: a 0.45
/// client ignores the notification — correctly — and therefore never learns
/// a handoff was coming, so it is left reconnecting against a dead socket.
/// Nothing breaks; it simply does not get what 0.46 exists to give.
/// **0.47.0** (roadmap item 11): `rar` enters [`ARCHIVE_FORMATS`](crate::ARCHIVE_FORMATS)
/// (`crates/norte-proto/src/vpath.rs`). The whitelist decides which
/// composite schemes a client can FORM, so widening it is a wire change even
/// though it does not move a single byte of an existing message.
///
/// Window N=0.47.x / N-1=0.46.x, and the asymmetry is the usual one: a 0.46
/// client does not OFFER `rar+file://…` because its own whitelist does not
/// carry it, so it simply does not see the feature.
///
/// What that client DOES do — and the first draft of this paragraph said the
/// opposite (#247) — is PARSE one that reaches it: [`crate::VPath::parse`]
/// does not consult [`ARCHIVE_FORMATS`](crate::ARCHIVE_FORMATS); only
/// `archive_compose` does. A 0.46 that receives `rar+file:///a.rar/!/x` via
/// a bookmark, the history, or a session body ends up with a scheme it does
/// not know and a literal `!` segment, and fails downstream when it asks for
/// it. Nothing gets corrupted and the bump remains MINOR; what does not hold
/// is the reasoning that it never gets formed.
///
/// The other way around, a 0.47 client against a 0.46 daemon does form the
/// path — and the old daemon responds `Unsupported` via its dispatch `_`
/// arm, which is the honest answer — because [`version_compatible`] does not
/// negotiate a client with a minor GREATER than the server's: the connection
/// does not even get established. Additive, therefore, MINOR.
/// **0.48.0** (L2, the UI session): [`SESSION_GET`] and [`SESSION_PUT`], with
/// [`Session`], [`SessionGetResult`], [`SessionPutParams`] and
/// [`SessionPutResult`]. Additive: no existing message changes shape, and
/// the session body is OPAQUE to this crate and to the core.
/// **0.49.0** (#139, #140): [`FS_DIR_SIZE`] with [`FsDirSizeParams`] and
/// [`TaskKind::DirSize`](crate::TaskKind::DirSize), and [`CONNECTION_CLOSE`]
/// with [`ConnectionCloseParams`]/[`ConnectionCloseResult`]. Two methods in a
/// single bump because the branch never got published separately. Additive:
/// `dir_size` delivers its result via a Task's PROGRESS instead of a new
/// type, and `connection.close` closes by PATH instead of a session key the
/// frontend has no reason to know.
/// **0.50.0** (#132, writing files): [`ARCHIVE_PACK`], [`ARCHIVE_TEST`],
/// [`ARCHIVE_TEST_REPORT`], [`FILE_SPLIT`] and [`FILE_COMBINE`] — FIVE
/// methods —, with [`ArchivePackParams`], [`ArchiveTestParams`],
/// [`ArchiveTestResult`], [`ArchiveTestFailure`], [`ArchiveTestReportParams`],
/// [`FileSplitParams`], [`FileCombineParams`], [`ArchiveFormat`] and four
/// new [`TaskKind`](crate::TaskKind) variants.
///
/// Additive, and it does not touch the archive provider: none of the four
/// writes INSIDE a container — that would still be `READ_ONLY` (ADR 0018) —
/// all four manufacture new files. Unpacking is not here because it does not
/// need to be: it is an `fs.copy` from inside the container, which already
/// works.
///
/// Window N=0.50.x / N-1=0.49.x: a 0.49 client does not know the methods and
/// does not call them; a 0.50 client against a 0.49 daemon gets
/// `METHOD_NOT_FOUND`, which the `Backend` translates to
/// [`Error::Unsupported`](crate::Error) — "your daemon is older", not a
/// generic failure.
/// **0.51.0** (#247): neither a new type nor a new field — what changes is
/// what [`SESSION_PUT`] ACCEPTS, and that is why it is a bump.
///
/// A `put` whose `version` this core does not know how to read is now
/// refused with [`Error::Unsupported`](crate::Error), and `0` counts as
/// unknown. Before, it was accepted: the core dumped to disk a document that
/// its own load guard rejects, so from the next startup on the session was
/// stuck "from the future" forever — ownerless, unpersisted — until someone
/// deleted the file by hand. A newer `ntc` against an older `norte` was
/// enough: the two `SCHEMA_VERSION`s live in different crates and only a
/// test ties them together.
///
/// And the body's schema is declared by [`SessionPutParams::version`] and
/// NOTHING else: norte's frontends also stuffed an undocumented copy inside
/// the `body`, and that was the only one their reader looked at. A
/// third-party client doing what this contract says — `version: 2` in the
/// envelope, v2 body — reached a reader that saw it absent, took it for 0,
/// swallowed the fields it did not understand and rewrote them lost. The
/// reader now takes the GREATER of the two, so an old body with its copy
/// inside still reads the same.
///
/// Window N=0.51.x / N-1=0.50.x: a 0.50 client sends `version: 1` as always
/// and notices nothing; a 0.51 client against a 0.50 daemon does not either
/// — the old daemon accepts what it used to accept. What is lost against
/// the old one is the protection, not the functionality.
/// **0.52.0** (#163): [`SyncBlockerKind::IllegalDestName`], a name the
/// DESTINATION cannot have.
///
/// Nothing checked that a name legal under the source root was also legal
/// under the destination's, so `CON`, `f:ads` or a trailing dot — all three
/// legal on ext4 — got discovered while EXECUTING. The worst is `f:ads`: on
/// NTFS it works and writes an alternate stream, so the copy says it went
/// fine and the file is not there. Now the destination's provider decides,
/// since it is the one that knows its rules, and it comes out as a PLAN
/// blocker — where a human can do something about it.
///
/// Additive: [`SyncBlockerKind`] is `#[non_exhaustive]` with
/// `#[serde(other)] Unknown`, so a 0.51 client paints the blocker as
/// "unknown class" and **does not approve the plan**, which is exactly what
/// has to happen — a blocker that is not understood keeps blocking.
///
/// Window N=0.52.x / N-1=0.51.x: a 0.51 daemon does not emit the class and a
/// 0.51 client degrades it. What is lost against the old one is the check,
/// not correctness.
///
/// `0.53.0` (#251, #265, #282, ADR 0071): three OPTIONAL fields, grouped into
/// one bump because each would only have cost its own window otherwise.
/// [`crate::TaskProgress::unreadable`] says how many subtrees could not be
/// read — `fs.dir_size` counted them locally and threw them into a log, so
/// it answered with a confident, short total; [`PluginLoadError::dir_bytes`]
/// carries the basename's bytes, which until now crossed already converted
/// via an unmarked `to_string_lossy`; and
/// [`PluginSetApprovalParams::expected_digest`] makes what is granted be
/// what the human read.
///
/// Window N=0.53.x / N-1=0.52.x. All three are omitted when there is
/// nothing to say, so the ordinary JSON does not change, and what a 0.52
/// peer loses is a CHECK and not correctness. With one nuance ADR 0071
/// records: `expected_digest`'s degradation is safe because a 0.52 daemon
/// discovers the catalogue ONCE at startup and therefore has no window to
/// exploit — that is a property of that implementation, not of the
/// protocol, and no client can verify it.
///
/// `0.54.0` (#295, ADR 0073): [`FsListResult::dir_anchor`] and the
/// [`FsCopyParams::dest_anchor`] / [`FsMoveParams::dest_anchor`] that returns
/// it — the OPAQUE identity of the directory the human was looking at when
/// they approved, traveling with the request that writes into it.
///
/// Closes what ADR 0072 leaves open: a symlink **already planted** by the
/// time the core first looks is, from the core's point of view,
/// indistinguishable from a legitimate `~/copies -> /mnt/disk/copies`.
/// Outside the core there is something that does distinguish it: the human
/// was not looking at THAT node.
///
/// Window N=0.54.x / N-1=0.53.x. Both fields are omitted when there is
/// nothing to say, so the ordinary JSON does not change. A 0.53 daemon does
/// not emit an anchor, so a 0.54 client has none to return and does not send
/// the field; a 0.53 client against a 0.54 daemon does not send it either.
/// In both cases the CHECK is lost, not correctness, with the same ADR 0071
/// nuance: whoever does not receive it cannot know it did not happen, and
/// that is why the peer's version — which the SDK has retained since #294 —
/// is what decides whether it can be promised.
///
/// `0.55.0` (#279): [`crate::Error::ApprovalGone`], which says WHICH of the
/// three forms of "that approval is no longer there" occurred — `unknown`,
/// `expired` or `already-decided`. Before, all three came out as an
/// `INVALID_PARAMS` with the reason inside an English `message`, so a
/// frontend could only say "your click did not arrive": true in one case and
/// a lie in the other two, on a security surface.
///
/// Window N=0.55.x / N-1=0.54.x. Additive: the variant is new and `Error` is
/// `#[non_exhaustive]`, so a 0.54 client deserializes it to
/// [`crate::Error::Unknown`] and degrades to "generic error" — exactly what
/// it did before with `INVALID_PARAMS`.
///
/// `0.56.0` (#264): new method [`CONNECTION_LIST`] → [`ConnectionListResult`],
/// the named connections the daemon has configured. It exists so a frontend
/// can offer a selector without reading `connections.toml` itself — reading
/// it would force the entire network stack into a binary that only wants to
/// paint names.
///
/// Window N=0.56.x / N-1=0.55.x. Additive: a 0.55 client does not know the
/// method and does not call it. What is lost against an N-1 daemon is the
/// selector, not the ability to connect — going to a URL still establishes
/// the session the usual way, and that did not change.
///
/// `0.57.0` (#290): new method [`FS_CREATE`] ([`FsCreateParams`] →
/// [`FsTaskResult`], the same Task shape as `fs.mkdir`) and variant
/// [`crate::TaskKind::Create`]. Creates an EMPTY file, and only that: it is
/// what a terminal-less frontend was missing to offer "edit a new one",
/// which the TUI solves by launching `$EDITOR` and letting the editor create
/// the file on save.
///
/// Fails if the destination EXISTS ([`crate::ConflictKind::Exists`]), just
/// like `fs.mkdir`: there is no reading of "create" that means "empty out
/// whatever is there", and a method that silently truncates is data loss
/// with an innocent name.
///
/// [`FsCreateParams`] carries an optional `dest_anchor` (#295): `fs.mkdir`
/// does not have it and this method does, because it is the only one whose
/// success hands a path to a program outside norte. It is omitted when
/// there is none, so the JSON of whoever did not list the directory does not
/// change.
///
/// With it the policy's op-kind VOCABULARY grows: `create` joins
/// `copy|move|delete|mkdir` in [`RequestScopeParams::ops`] and in
/// [`PolicyApprovalRequired::op`]. That is also wire, even though it changes
/// no type: a 0.56 daemon DISCARDS `create` from a requested scope —
/// fail-closed, no error — so a 0.57 agent against an old daemon gets the
/// scope without that op and its creations get denied.
///
/// Window N=0.57.x / N-1=0.56.x. Additive: a 0.56 client does not know the
/// method and does not call it, and degrades the new kind to
/// `TaskKind::Unknown` via its `serde(other)` — nothing to gate on emission.
/// The combination the handshake DOES allow is that one — a 0.56 client
/// against a 0.57 daemon — and not the reverse: a client from the future is
/// rejected outright (see [`version_compatible`]).
///
/// `0.58.0` (#250): new method [`ARCHIVE_PACK_REPORT`]
/// ([`ArchivePackReportParams`] → [`ArchivePackReportResult`]), a twin of
/// [`FS_RENAME_BATCH_REPORT`] and for the same reason: **there is a true
/// fact about what was just written that does not fit in the Task's
/// outcome.**
///
/// Here that fact is names that **mean something else outside**: `a\b` is a
/// directory separator in 7-Zip and in Explorer, `f:ads` opens an alternate
/// stream on NTFS, `CON` cannot be extracted on Windows at all, and a
/// trailing dot or space gets eaten by Windows without saying so. Our own
/// reader round-trips them exactly, which is exactly why the round-trip test
/// sees none of them.
///
/// **Why it WARNS instead of rejecting, when its sibling does reject.** Two
/// entries that fold to the same name do not get packed: `archive.pack`
/// fails with [`crate::ConflictKind::Exists`], because extracted somewhere
/// else one of the two files DISAPPEARS and an archive's destination is
/// unknown by definition. This is a different thing: `a\b.txt` extracted on
/// Linux is still `a\b.txt`, and on Windows it is a file `b.txt` inside a
/// folder `a`. Nothing is lost; it lands somewhere different. Rejecting it
/// would take down perfectly legitimate Unix trees to prevent something that
/// is not even a loss.
///
/// And warning needs this method, because `task.progress` has no channel for
/// it: its `unreadable` counts something else, and a counter that meant two
/// different things depending on the task is one nobody could read.
///
/// Window N=0.58.x / N-1=0.57.x. Additive, and the loss has to be counted in
/// the direction the handshake PERMITS, which is only one: **0.57 client
/// against 0.58 daemon**. The reverse does not happen — a client from the
/// future is rejected outright in `initialize` (see [`version_compatible`]),
/// so "a 0.58 client against a 0.57 daemon" is not a degraded scenario but a
/// connection that never comes to exist.
///
/// That 0.57 client does not know the method and does not call it. **What
/// it loses is the warning, not the archive**: the `.zip` is written the
/// same, with the same entries and the same bytes, and whoever extracts it
/// on Windows will find the entries placed somewhere they were not left,
/// without anyone having told them.
/// `0.59.0` (#311): new methods [`FS_CHECKSUM`] ([`FsChecksumParams`] →
/// [`FsTaskResult`]) and [`FS_CHECKSUM_REPORT`] ([`FsChecksumReportParams`] →
/// [`FsChecksumReportResult`]), plus [`TaskKind::Checksum`](crate::TaskKind).
///
/// Checking a file against a sum someone published is the only way there is
/// to know that what got downloaded is what was offered, and norte did not
/// have it: all three reference managers do (Krusader puts it in its File
/// menu). It reads CONTENT and mutates nothing.
///
/// **Why two methods and not one.** The result is N digests, and that does
/// not fit in a Task's outcome nor in its progress, which only knows how to
/// count. It is the same split — and for the same reason — as
/// [`FS_RENAME_BATCH_REPORT`] and [`ARCHIVE_PACK_REPORT`]: the Task does the
/// work and the report says what came out.
///
/// Window N=0.59.x / N-1=0.58.x. Additive, and the loss is counted in the
/// direction the handshake PERMITS, which is only one: **0.58 client against
/// 0.59 daemon**. The reverse does not happen — a client from the future is
/// rejected outright in `initialize` (see [`version_compatible`]).
///
/// That 0.58 client does not know the methods and does not call them, so
/// **what it loses is the whole check**: there is no partial degradation to
/// count, nor a field silently ignored. The only thing it can see of this
/// bump is a foreign [`TaskKind::Checksum`](crate::TaskKind) Task in
/// `task.list`, which degrades to `Unknown` via its `serde(other)` — it sees
/// it run and cannot name it, exactly what already happens to it with
/// `Compare` or `DirSize`.
/// `0.60.0` (#314): new method [`FS_SET_MODE`] ([`FsSetModeParams`] →
/// [`FsTaskResult`]), [`TaskKind::SetMode`](crate::TaskKind) and the
/// capability [`CapabilityFlags::POSIX_MODE`](crate::CapabilityFlags).
///
/// It is the only category where all three reference managers TOUCH and
/// norte only LOOKED: properties showed the permissions and there was no way
/// to change them, because the protocol had no way through. Krusader edits
/// them from its properties dialog (numeric permissions included).
///
/// **It is a full MUTATION**, not a setting: it goes through policy, leaves
/// a journal entry with its reversal — the PREVIOUS mode, read before
/// writing the new one — and is a cancelable Task, because a batch of a
/// thousand files is one.
///
/// POSIX permissions only, and on purpose. Dates and ownership are two other
/// questions: `mtime` is easy to promise and hard to undo well, and changing
/// the owner requires privileges norte does not ask for. Each will arrive
/// with its own method and its own permission, not as an optional field of
/// this one.
///
/// Window N=0.60.x / N-1=0.59.x. Additive, and the loss is counted in the
/// direction the handshake permits — **0.59 client against 0.60 daemon**:
/// that client does not know the method and does not call it, so it is left
/// unable to change permissions and with the same read-only surface it had.
/// Of the new capability it sees nothing: unknown flags are ignored when
/// parsing (ADR 0004), so a provider advertising `POSIX_MODE` reads to it as
/// if it did not advertise it, which is exactly what it is fine for it to
/// believe.
///
/// The third thing that does reach it: a foreign
/// [`TaskKind::SetMode`](crate::TaskKind) Task in `task.list`, which degrades
/// to `Unknown` via its `serde(other)` — it sees it run and cannot name it,
/// like already happens with `Compare`, `DirSize` or `Checksum`.
/// `0.61.0` (#314): [`ApprovalDetail`], and with it the `detail` field of
/// [`PolicyApprovalRequired`] and [`PendingApproval`].
///
/// The approval carried the op and the paths, and for every op but one that
/// IS the decision. `set-mode` is the first where two requests with the
/// SAME op and the SAME paths mean opposite things — `0600` and `4777` —
/// so the human could not see what they were saying yes to. It is the same
/// argument [`PolicyApprovalRequired::paths_total`] makes for the count, and
/// that is why the field goes in both forms: the notification and
/// `policy.pending`'s resync.
///
/// Window N=0.61.x / N-1=0.60.x. Additive: the field is omitted when it says
/// nothing, so the JSON of the other ops does not change by a byte, and a
/// 0.60 client ignores it. **What it loses is exactly what the field
/// adds**: faced with an agent's `set-mode`, a 0.60 frontend still asks
/// "set-mode on 12 paths" without being able to say what the mode was.
/// `0.62.0` (#315, #121): two fields on `fs.set_mode` and one on
/// `ai.rename_plan`, and all three are the same kind of thing — telling the
/// core WHAT it acts on, instead of having it guess.
///
/// [`FsSetModeParams`] gains `recursive` and `dir_mode`. The first is the
/// gap ADR 0081 deliberately deferred: changing permissions "of this folder
/// and what is inside it" is what all three reference managers offer, and
/// until now `fs.set_mode` changed EXACTLY the paths it was given. The
/// second exists because `chmod -R 644` over a tree leaves it unusable —
/// without the execute bit on a directory you cannot even enter it — so the
/// DIRECTORIES' mode goes separately; absent, it is the same for everything,
/// which is what `chmod -R` does and what breaks trees.
///
/// [`AiRenamePlanParams`] gains `names`: the basenames the plan is requested
/// for. Empty = the whole directory, which is what it used to do. With
/// first-class selection (#103), marking five files and requesting a plan
/// sent the directory's thousand files to the provider — more than what the
/// human pointed at, which is exactly what the AI gate exists to bound.
///
/// Window N=0.62.x / N-1=0.61.x. Additive in both directions the handshake
/// permits, and the loss is counted for a **0.61 client against a 0.62
/// daemon**: it does not know the fields, does not send them, and so
/// `fs.set_mode` acts on the exact paths — 0.61's behavior, which is what
/// that client already expects — and `ai.rename_plan` keeps sending the
/// whole directory. Neither of the two is a check that stops being done:
/// they are scopes that do not narrow.
/// `0.63.0` (#325): asking whoever is at the keyboard for a connection's
/// secret.
///
/// Adds [`Error::SecretNeeded`](crate::Error::SecretNeeded) and the method
/// [`CONNECTION_PROVIDE_SECRET`]. It is the same flow as host keys' TOFU,
/// and for the same reason: the secret resolver lives in `norte-connect`,
/// has no user interface and must not have one, so the only way for a human
/// to answer is for the question to go up the wire. Until now a secret that
/// was not in the environment, the keyring or `secrets.age` was a connection
/// that could not be opened.
///
/// **A secret crosses the socket for the first time**, and that was decided
/// deliberately: ADR 0015 (2026-09-01 amendment) says what crosses, over
/// what, and why it is accepted. In short: it is a 0600 UDS owned by the
/// user themself, and whoever can read it can already read
/// `/proc/<pid>/environ`, which is where the `NORTE_SECRET_*` variable lives
/// today. The threat model does not get worse.
///
/// Window N=0.63.x / N-1=0.62.x. Additive: nothing in the JSON of existing
/// operations changes. The loss, for a **0.62 client against a 0.63
/// daemon**: it does not know `SecretNeeded`, so it degrades it to `Unknown`
/// and shows an error where the new client would open a dialog. It is not a
/// check that stops being done nor a scope that widens — it is exactly what
/// that client already did: being unable to open that connection.
///
/// **What does hurt is the way back, and it is not in the wire.**
/// `ConnectionSpec` is `deny_unknown_fields`, so a 0.62 binary reading a
/// `connections.toml` that already carries `secret = "prompt"` does not
/// fail that one connection: it fails the WHOLE file, and with it every
/// other connection. It only happens when downgrading, or with a mixed
/// install (a stale `ntc` from `cargo install` next to a new window — it has
/// already happened in this repository). Removing the key from the file
/// fixes it.
///
/// `0.64.0` (#322): WHY a connection could NOT be established reaches
/// whoever is watching.
///
/// Adds the [`CONNECTION_FAILED`] notification and its
/// [`ConnectionFailed`]. Until now, a connection failure reached the
/// frontend as a taxonomy CATEGORY — almost always `PermissionDenied` — and
/// the sentence saying what happened was written to the daemon's log and
/// thrown away: "permission denied" was indistinguishable from a wrong key,
/// a mistyped passphrase, or a bucket with no permissions. And with the
/// embedded CLI it was readable, because `tracing` comes out on the
/// process's own stderr, so the same failure got diagnosed or not depending
/// on the TRANSPORT.
///
/// It travels as a notification and not inside the error because the
/// taxonomy deliberately carries no free text (see [`crate::Error`]): with
/// the error you DECIDE, and you decide by category. It is the same shape
/// as [`CONNECTION_DEGRADED`], which is this repository's precedent for a
/// connection condition with a human explanation.
///
/// Window N=0.64.x / N-1=0.63.x. Additive: it does not change the JSON of
/// any existing operation. The loss, for a **0.63 client against a 0.64
/// daemon**: it does not know the notification and silently discards it
/// (ADR 0004), meaning it stays exactly as it was — the failure keeps
/// reaching it as a category, without the sentence. It is not a check that
/// stops being done nor a scope that widens.
///
/// The opposite direction — **0.64 client against a 0.63 daemon** — never
/// happens: [`version_compatible`] does not negotiate a client minor
/// GREATER than the server's, so that client dies in `initialize` with
/// `VERSION_MISMATCH` and never gets to wait for the notification. The mixed
/// case this repository has already run into — a stale `cargo install` — is
/// seen as a connection that refuses, not a silent degradation.
///
/// `0.65.0` (#328): the DAEMON's log can be read from outside.
///
/// Adds [`LOG_TAIL`] and [`LOG_LEVEL`], with [`LogTailParams`],
/// [`LogTailResult`], [`LogLevelParams`], [`LogLevelResult`] and the wire
/// shape of a line, [`LogLine`]. Until now a frontend's log panel painted
/// the ring of ITS OWN process, and with a separate daemon — which is
/// normal for the window, which starts its own (#300) — that ring has the
/// bridge's and the renderer's lines, while the providers, the journal, the
/// policy and the reason a connection failed are on the other side of the
/// socket. The panel was not broken: it was watching the wrong process, and
/// since #326 it says so. This is the other half.
///
/// **The log is PULLED with a cursor; the daemon does not push it.** The
/// ring already carries a monotonic counter, so the cursor costs nothing to
/// produce and the daemon keeps no per-client state — no subscription to
/// register, no unsubscribe that gets lost when a client dies. A
/// notification that drops is a silent gap; a stale cursor is arithmetic,
/// and the response says exactly how many lines fell behind. A closed panel
/// costs nothing, which is where most of the time is spent.
///
/// **The level is raised by METHOD, not by parameter.** The ring's bound —
/// `suppaftp` writes `PASS <password>` at TRACE (#43, rule 10) — lives in
/// the process that owns the ring, so the only way for it to survive the
/// socket is for the client to ASK for a level and the daemon to be the one
/// applying it. There is no second copy of the whitelist to keep in sync.
///
/// Window N=0.65.x / N-1=0.64.x. Additive: it does not change the JSON of
/// any existing operation. The loss, for a **0.64 client against a 0.65
/// daemon**: it does not call the methods and its panel is left with the
/// local ring — what #326 built — meaning exactly what it already had.
///
/// **The opposite direction does not exist.** A 0.65 client against a 0.64
/// daemon never gets to try `log.tail`: [`version_compatible`] does not
/// negotiate a client minor GREATER than the server's, so that client dies
/// in `initialize` with `VERSION_MISMATCH`. Writing it as "it asks for the
/// method and gets told it does not exist" would describe a branch that
/// cannot run, and a frontend coding it would be writing dead code.
///
/// What DOES happen, and what the frontend has to handle, is a daemon **of
/// this very version** compiled without the `logging` feature: it knows the
/// methods and has no ring to serve, so it answers `Unsupported`
/// (`-32000`) — the daemon DISPATCHES them, so `METHOD_NOT_FOUND` is not an
/// answer it can give; the SDK folds it the same way defensively anyway.
/// That is the only condition under which a peer that HAS completed the
/// handshake can refuse these two methods, and facing it the panel degrades
/// to the local ring **saying why**: a panel left empty without an
/// explanation is indistinguishable from a daemon that did nothing, which is
/// exactly the confusion #326 started to fix.
///
/// # 0.66.0 — a span's background and the viewer's width (D4, ADR 0037)
///
/// Two optional fields: [`SpanWire::bg`] and
/// [`PluginPreviewStyledParams::columns`]. The image previewer needs them:
/// it paints each pair of pixels as a half block with the top one in `fg`
/// and the bottom one in `bg`, and it has to know how many cells to shrink
/// to.
///
/// Window N=0.66.x / N-1=0.65.x. Additive: without `bg` a span's JSON is
/// the same as before, and without `columns` the request is the same as
/// before. The loss, for a **0.65 client against a 0.66 daemon**: it does
/// not send `columns`, so the guest picks its own default width and the
/// viewer crops; and it reads a span with `bg` ignoring the field (ADR
/// 0004), so an image paints with half its pixels — readable as color
/// blocks from the top, with no error and no warning. A 0.66 client against
/// a 0.65 daemon does not negotiate.
///
/// # 0.67.0 — a `renamer` plugin's plan (C3, ADR 0095)
///
/// One method, [`PLUGIN_RENAME_PLAN`], and one field:
/// [`PluginCommandInfo::kind`], which distinguishes in
/// [`PluginInfo::commands`] a command that runs from a renamer that
/// proposes. The result is [`AI_RENAME_PLAN`]'s, because it is the same
/// plan from a different producer.
///
/// Window N=0.67.x / N-1=0.66.x. Additive: `kind` is omitted when it is
/// `command`, so a `PluginInfo` without renamers is byte-for-byte the old
/// one. The loss, for a **0.66 client against a 0.67 daemon**: it reads
/// `kind` ignoring it (ADR 0004) and shows a renamer as if it were a
/// command; running it asks for `plugin.run_command` with its id, and the
/// daemon answers `INVALID_PARAMS` ("the plugin does not run commands")
/// BEFORE instantiating anything — the palette shows the error, it renames
/// nothing. A 0.67 client against a 0.66 daemon does not negotiate.
///
/// # 0.68.0 — the reason a plan was refused (#332)
///
/// One field: [`AiRenamePlanResult::refused`], the sentence with which a
/// `renamer` plugin refused to propose, masked and bounded by the daemon.
/// Before, [`PLUGIN_RENAME_PLAN`] plainly answered `Unsupported` and the
/// sentence stayed in the log: "approve my `location` capability" reached
/// the reader as "not supported here". A refusal is no longer a wire error:
/// it is an empty plan with a reason.
///
/// Window N=0.68.x / N-1=0.67.x. Additive: the field is omitted when there
/// is no reason, so every plan that already existed is byte-for-byte the
/// old one. The loss, for a **0.67 client against a 0.68 daemon**: it
/// reads `refused` ignoring it (ADR 0004) and shows "the model did not
/// propose changes" where the plugin explained why — an imprecise message,
/// not an error. A 0.68 client against a 0.67 daemon does not negotiate.
///
/// # 0.69.0 — hooks speak (ADR 0100)
///
/// One notification: [`PLUGIN_NOTICE`], with [`PluginNotice`]. A `hook`
/// plugin watches the entries the journal already recorded and can return a
/// sentence for the human; the daemon masks it, bounds it and broadcasts it
/// only to humans, attributed to the plugin. The same notification says
/// when the daemon shut down the hooks of a plugin that failed three times
/// in a row.
///
/// Window N=0.69.x / N-1=0.68.x. Additive: no existing message changes. The
/// loss, for a **0.68 client against a 0.69 daemon**: the notification is
/// discarded (ADR 0004), so the hook runs — the source is the journal, not
/// the frontend — and its sentence reaches nobody; and it also does not
/// learn that a plugin's hooks got shut down. A 0.69 client against a 0.68
/// daemon does not negotiate.
///
/// # 0.70.0 — policy says no to a sidecar (ADR 0101)
///
/// One new value in [`PLUGIN_NOTICE_KINDS`]: `effect-denied`. A hook can ask
/// the daemon to write a sidecar next to what changed; the daemon writes it
/// as actor `plugin` through the policy engine, and if a human rule denies
/// it, it is told once per plugin. No `text`.
///
/// In the same bump, [`PluginInfo::capabilities`] — an open list — stops
/// carrying the fixed `fs-write` badge and carries one per name,
/// `fs-write:<name>` (like `provider:<scheme>` and `location-root:<mark>`).
///
/// Window N=0.70.x / N-1=0.69.x. Additive: `PluginNotice`'s shape does not
/// change. The loss, for a **0.69 client against a 0.70 daemon**: it sees a
/// `kind` it does not know and without `text`, and discards it as the
/// contract dictates — it does not learn that its own policy is stopping a
/// plugin from writing; and a client that treated the exact `fs-write`
/// badge specially stops doing so. A 0.70 client against a 0.69 daemon does
/// not negotiate.
///
/// # 0.71.0 — uninstalling over the wire (ADR 0104)
///
/// A new method, [`PLUGIN_UNINSTALL`]: the window's and the terminal's
/// extension managers uninstall without going through the CLI. It deletes
/// the plugin's directory and leaves its state off and unapproved — the
/// same thing `norte plugin uninstall` already did on disk, now also on the
/// daemon's IN-MEMORY registry, which until now kept listing the deleted
/// one until restart. User connections ONLY.
///
/// Window N=0.71.x / N-1=0.70.x. Additive: no existing message changes. The
/// loss, for a **0.70 client against a 0.71 daemon**: none — it does not
/// know how to ask for it and does not ask. A 0.71 client against a 0.70
/// daemon does not negotiate.
///
/// # 0.72.0 — the icon column (ADR 0105)
///
/// Two optional fields: [`PluginDecorateParams::kinds`], the class of each
/// path being decorated (an icon decorator needs to know which is a
/// folder), and [`PluginDecorations::slot`], which slot of the row paints
/// what a decorator returns — `icon` to the left of the name, `badge` to
/// the right — which comes from the manifest. In the same bump,
/// `norte:plugin` goes up to 0.10.0: `decorate` receives name and class.
///
/// Window N=0.72.x / N-1=0.71.x. Additive: no existing message changes
/// shape, and a badge decorator travels byte-for-byte the same. The loss,
/// for a **0.71 client against a 0.72 daemon**: it does not send `kinds`,
/// so an icon decorator sees everything as `other` and folders go without
/// an icon; and it does not read `slot`, so it paints the icon on the
/// right, like a badge, and the first plugin to answer covers the other.
/// A 0.72 client against a 0.71 daemon does not negotiate.
///
/// # 0.73.0 — `plugin.thumbnail` (ADR 0107)
///
/// A new method: a file's thumbnail by the first consented `thumbnail`
/// plugin whose mimetype matches ([`PluginThumbnailParams`],
/// [`PluginThumbnailResult`]). Additive: no existing message changes. A
/// **0.73 client against a 0.72 daemon** gets `MethodNotFound` and treats
/// it as "no thumbnail", which is what there was. A 0.72 client against a
/// 0.73 daemon does not ask for it.
/// # 0.74.0 — `plugin.panel_render` (phase 3 of the 2026-09-15 program)
///
/// A new method: the FRAME a `panel` plugin paints in a layout slot
/// ([`PluginPanelRenderParams`], [`PluginPanelRenderResult`]), and
/// [`PluginInfo`] gains `panels` (`Vec<`[`PluginPanelInfo`]`>`) so a
/// frontend knows which slots a plugin offers before opening them.
/// Additive: no existing message changes, and the new field has a
/// `default`.
///
/// The result travels with `#[serde(flatten)]`, so "no frame" is `{}` on
/// the wire and NOT `null` — same as its `plugin.preview*` twins.
///
/// A **0.74 client against a 0.73 daemon** gets `MethodNotFound` and treats
/// it as "this panel cannot be painted" — the slot keeps its notice, the
/// same thing that happens with an uninstalled plugin. And it sees an empty
/// `panels`, meaning no panel to offer, which is what there was. A 0.73
/// client against a 0.74 daemon does not ask for either of the two things.
/// # 0.75.0 — `fs.dir_usage` (phase 4 of the 2026-09-15 program)
///
/// Two new methods and a new Task class: what a directory is MADE of,
/// child by child ([`FsDirUsageParams`] → [`FsTaskResult`], and
/// [`FsDirUsageReportParams`] → [`FsDirUsageReportResult`]), plus
/// [`TaskKind::DirUsage`](crate::TaskKind). Additive: no existing message
/// changes shape.
///
/// The task+report split is [`FS_CHECKSUM`]'s, and for the same reason: the
/// list of children does not fit in a Task's outcome, and progress only
/// knows how to count. `fs.dir_size` still exists and still answers its own
/// question — ONE number over a selection — which is a different question.
///
/// The loss, for a **0.74 client against a 0.75 daemon**: it does not know
/// how to ask for `fs.dir_usage` and does not ask, so it is left without a
/// disk map — the screen it had. And if it sees ANOTHER client's Task in
/// `task.list`, its `TaskKind` falls into `Unknown` via `serde(other)`: it
/// paints it with its progress and its cancel button, but with no name,
/// just "task". This is what happens to any new class, and it is why
/// 0.49.0 and 0.59.0 bothered giving theirs its own label.
///
/// The other direction does NOT count: a 0.75 client against a 0.74 daemon
/// never gets to ask for anything, because [`version_compatible`] rejects a
/// client with a minor greater than the server's and it dies in
/// `initialize`.
/// # 0.76.0 — the journal's timeline (phase 7 of the 2026-09-15 program)
///
/// Two new methods: [`JOURNAL_LIST`], which pages the journal backward, and
/// [`JOURNAL_UNDO_AFTER`], which undoes the HUMAN's work after a `seq` and
/// returns a Task with `policy.undo_report`'s report. Additive: no existing
/// message changes shape.
///
/// Both refuse an AGENT connection before parsing their params: the journal
/// names everything that has been touched on this machine, which for a
/// bounded agent is an existence oracle, and undoing the human's work is
/// not the agent's decision to make.
///
/// A **0.76 client against a 0.75 daemon** gets `MethodNotFound` on both
/// and is left without a timeline panel — the screen it had. A 0.75 client
/// against a 0.76 daemon does not ask for them.
/// # 0.77.0 — organizing a directory (phase 8, ADR 0122)
///
/// Three new methods: [`AI_ORGANIZE_PLAN`] and [`PLUGIN_ORGANIZE_PLAN`],
/// which PROPOSE a reviewable plan with destinations that may carry
/// folders, and [`FS_ORGANIZE`], which applies it as ONE undoable batch.
/// Plus [`PluginCommandKind::Organizer`], so a frontend learns a plugin
/// brings an organizer.
///
/// The enum variant is the only part that is not purely additive, and it is
/// the same exposure 0.67.0 accepted with `renamer`: the `kind` field is
/// omitted when it is `command`, so an old peer only sees it if the plugin
/// truly declares an organizer, and then fails to deserialize THAT
/// `PluginInfo`.
///
/// A **0.77 client against a 0.76 daemon** gets `MethodNotFound` and is
/// left unable to organize, which is what there was. A 0.76 client against
/// a 0.77 daemon does not ask for it.
/// # 0.78.0 — releasing the UI session (phase 9 of the 2026-09-15 program)
///
/// A new method, [`SESSION_RELEASE`]: the connection that OWNS the UI
/// session gives up ownership without disconnecting. This is what makes
/// the handoff between frontends possible (`app.handoff`) — dump the
/// screen, release it, and have the other one claim it in its
/// `session.get` — and until now that only happened on disconnect.
///
/// Additive: no existing message changes shape. Releasing someone ELSE's
/// does nothing and says so ([`SessionReleaseResult::released`]), instead
/// of pretending it happened: one connection does not evict another.
///
/// A **0.78 client against a 0.77 daemon** gets `MethodNotFound`, and the
/// handoff degrades honestly: nothing gets released, so the other frontend
/// is not launched to claim something it will not be able to have. A 0.77
/// client against a 0.78 daemon does not ask for it.
/// # 0.79.0 — `policy.undo_report` answers `NotFound` for an id it does not know
///
/// No type changes: a method's error RESPONSE changes. A `task_id` that was
/// never an undo, or that the ring already evicted, answers with the
/// taxonomy's `Error::NotFound`, the same as its twin
/// `fs.rename_batch_report` and the SDK's embedded arm. Until 0.78 it was
/// `INVALID_PARAMS` without `data`, which a client read as `Internal` — the
/// response of a provider that panics.
///
/// A **0.78 client built with the SDK against a 0.79 daemon** comes out
/// ahead: the SDK delivers `data` as-is, so it reads `NotFound` where it
/// used to read `Internal`. Only a hand-written client that distinguished
/// the case by the `-32602` code stops recognizing it. A 0.79 client
/// against a 0.78 daemon gets the usual `INVALID_PARAMS`.
/// # 0.80.0 — `journal.undo_after` gains a ceiling (`upto_seq`)
///
/// [`JournalUndoAfterParams::upto_seq`]: the newest `seq` the human saw
/// counted. The undo does not go past it, so whatever happened after
/// painting the timeline does not fall inside an "undo up to here" that
/// never counted it.
///
/// Additive and optional. A **0.80 client against a 0.79 daemon**: the
/// daemon would ignore the field (ADR 0004) and undo, with no ceiling, what
/// the question did not count, so the SDK does NOT send it — with a
/// ceiling requested it refuses with `Unsupported` and says so, like
/// `plugin.set_approval`'s anchor (#294). A **0.79 client against a 0.80
/// daemon** does not send it, and the daemon reads it as `None`: no
/// ceiling, which is 0.79's behavior.
///
/// # 0.81.0 — `fs.search` gains the filters that make a search useful
///
/// [`FsSearchParams`] grows by TEN fields, all optional: `kinds`,
/// `min_size`, `max_size`, `mtime_after`, `mtime_before`, `exclude_roots`,
/// `exclude_names`, `whole_word`, `recursive` and `encoding`. None changes
/// the JSON of a search that does not use them: the optional ones are
/// omitted, and `whole_word` and `recursive` are omitted when they hold
/// their default.
///
/// **What a peer one version behind loses has to be said plainly, because
/// it is not the usual case.** A filter the daemon ignores does not
/// silently stop filtering: it returns the SUPERSET. Whoever asked for
/// "under a meg, not descending into `node_modules`" receives the whole
/// tree and has no way to know that what they are looking at is not what
/// they asked for — a search that returns too much reads exactly like a
/// plain search. That is why the SDK **does not send them** to a 0.80
/// daemon: with any of them set it refuses with `Unsupported` and names the
/// filter, just like `journal.undo_after`'s ceiling. With none set there is
/// nothing to refuse and the search runs as it always did.
///
/// **That refusal is the SDK's, not the protocol's, and it has to be said
/// here** because this document is read by whoever writes a client from
/// `proto.schema.json`: the schema shows the ten fields and cannot show the
/// check. A client that is not this SDK has to look at the version it
/// negotiated in `initialize` itself before sending a filter, or it will
/// get the superset with no signal at all.
///
/// The other way around is harmless: a **0.80 client against a 0.81
/// daemon** does not send them, the daemon reads them absent, and that is
/// exactly 0.80's search. A NEWER client is a different thing, and a
/// different size: `kinds` is the first `Vec<EntryKind>` that travels in a
/// REQUEST, and that enum's `serde(other)` was designed for a client to
/// degrade a response. A class this daemon does not know will reach it as
/// `Other` and it will filter by it — meaning it NARROWS instead of
/// widening, which is the safe direction, but it is not "nothing happens".
///
/// # 0.82.0 — a task can be PAUSED (`task.pause`, `task.resume`)
///
/// [`TASK_PAUSE`] and [`TASK_RESUME`], with the same per-actor scope as
/// [`TASK_CANCEL`] (ADR 0147). `TaskState::Paused` has existed since M0 and
/// debuts here: an N-1 client already knows how to read it, and treats it
/// as non-terminal.
///
/// A **0.82 client against a 0.81 daemon** gets `METHOD_NOT_FOUND`, which
/// the SDK translates to `Unsupported`: pausing does not work and it is
/// SAID, not faked. A **0.81 client against a 0.82 daemon** does not ask
/// for pauses, and sees `Paused` on tasks another client paused — as
/// non-terminal, which is correct.
///
/// # 0.83.0 — the serial queue (`queued`, `task.move`)
///
/// [`FsCopyParams::queued`] and [`FsMoveParams::queued`], optional: what is
/// queued runs one at a time instead of up to four at once, which on a
/// mechanical disk is faster. And [`TASK_MOVE`] reorders what has not
/// started yet (ADR 0149).
///
/// A **0.83 client against a 0.82 daemon**: the daemon ignores `queued`
/// (ADR 0004) and the transfer runs in parallel — slower on a mechanical
/// disk, never incorrect — and `task.move` answers `METHOD_NOT_FOUND`,
/// which the SDK reports as `Unsupported`. A **0.82 client against a 0.83
/// daemon** does not send `queued`, it reads as `false`, and that is
/// exactly 0.82's behavior.
/// # 0.84.0 — the destination that disappears (`ConflictKind::DestinationGone`)
///
/// A new conflict subtype for when the destination directory stops being
/// where it was asked to be with the task already running: it was deleted,
/// moved or replaced while copying (ADR 0151).
///
/// It exists because the case arrived as plain `Error::NotFound`, which in
/// the middle of a copy of thousands of files the reader reads as "cannot
/// find something from the source". It is not
/// [`ConflictKind::EscapesRoot`](crate::ConflictKind::EscapesRoot): that one
/// says the path leads somewhere else via a symlink — an answer about the
/// path's shape, read as security — and this one says the folder is gone.
/// The remedy is different too: recreate it and retry.
///
/// A **0.83 client against a 0.84 daemon** degrades the subtype to
/// [`ConflictKind::Unknown`](crate::ConflictKind::Unknown) and shows plain
/// "conflict" (ADR 0005). **What it loses is the sentence, not the
/// protection**: the daemon is the one that checks, so the task fails
/// instead of saying it copied. What was already written does stay in the
/// folder that got deleted, the same for both (#369).
///
/// A **0.84 client against a 0.83 daemon** never gets to negotiate:
/// [`version_compatible`] accepts a client with a minor EQUAL to or one
/// behind, never ahead, and the daemon answers `VERSION_MISMATCH`. And it
/// is worth saying what that daemon used to do, because it was not
/// answering wrong: it marked the copy **`Completed`** with the files in
/// the trash (ADR 0151). This does not improve a message, it changes a
/// "fact" that was a lie into a failure — and there is nothing a client
/// can compensate for on its own.
pub const PROTOCOL_VERSION: &str = "0.84.0";

/// `initialize` — MANDATORY handshake before any other method
/// (ADR 0011). Rejects incompatible versions (see
/// [`version_compatible`]) and negotiates the encoding (today only `"json"`).
pub const INITIALIZE: &str = "initialize";
/// `daemon.shutdown` — shuts down the daemon: `graceful` (default) waits
/// for live tasks; without graceful it cancels them first. Authenticated
/// like everything else. Only a human connection (without `agent_session`)
/// can shut down: for an agent connection it is `INVALID_REQUEST`, like the
/// other acts of human governance (e.g. `policy.grant_scope`/`decide`/`undo_session`).
pub const DAEMON_SHUTDOWN: &str = "daemon.shutdown";
/// `task.list` — resync for a frontend that (re)connects (0.5.0, phase 3):
/// the LIVE tasks plus the recent outcomes the server retains
/// (bounded ring, best effort); later changes arrive via
/// [`TASK_PROGRESS`]. The receiver MUST deduplicate by `task_id` (the
/// same task can arrive both live and terminal in the same response if
/// both fall inside the ring's window).
///
/// Visibility by actor: a human connection sees ALL tasks; an
/// agent connection (`agent_session` in `initialize`) sees ONLY its
/// own session's — `current` carries other actors' paths and is not
/// crossed. The same criterion routes the [`TASK_PROGRESS`] notification.
pub const TASK_LIST: &str = "task.list";
/// `fs.read` — ONE chunk of a file, in base64 (0.5.0). For presentation
/// reads (viewer); copies NEVER go through here (they are daemon tasks).
/// The returned chunk may be shorter than requested: `eof` says whether
/// the file ended — if `false`, the caller repeats with the advanced
/// offset.
pub const FS_READ: &str = "fs.read";
/// `fs.capabilities` — capabilities of THE LOCATION a path names
/// (0.5.0; per location since 0.45.0, ADR 0054): the frontend decides e.g.
/// whether F8 offers a trash (ADR 0009).
///
/// It always took a path and until 0.44 answered the same for all of them,
/// which is false as soon as a machine mounts two different filesystems.
/// Since 0.45.0 the directory gives the answer: `CASE_SENSITIVE` and
/// [`CapabilityFlags::FULL_FOLD`](crate::CapabilityFlags::FULL_FOLD) can
/// differ between two paths of the same provider, while what belongs to the
/// backend — `READ_ONLY`, `TRASH`, `SYMLINKS` — comes out the same through
/// both doors.
///
/// Asking about a FILE answers for the directory that contains it: what
/// this response decides is whether two names can coexist there. And a path
/// that does not exist **is not an error**: it answers what the provider
/// declares, the same as before 0.45.0.
pub const FS_CAPABILITIES: &str = "fs.capabilities";

/// `host.volumes` — enumerates the HOST's volumes (0.37.0, #131): mount
/// point, filesystem type, kind and free/total space. It is deliberately
/// not an `fs.*` method (design §A of `2026-08-10-volumes-design.md`): a
/// volume is a property of the MACHINE, not of a path, so there is no
/// provider to ask.
///
/// ONLY a `User` connection gets an answer: the mount table names the
/// human's disks, servers and removable media, and an agent connection
/// (`agent_session` in `initialize`) sees it as `Error::PolicyDenied` —
/// information a path scope needs for nothing (design §C).
pub const HOST_VOLUMES: &str = "host.volumes";

/// `connection.list` — the NAMED connections the daemon has configured
/// (0.56.0, #264): `(name, url)` in alphabetical order.
///
/// It exists so a frontend can offer a connection selector **without
/// reading `connections.toml` itself**. Reading it would force the entire
/// network stack — russh, opendal, suppaftp, age, keyring — into a binary
/// that only wants to paint a list of names, and the daemon already has all
/// of that because it is the one that opens the sessions.
///
/// **Never a secret.** A `ConnectionSpec` REFERENCES its credentials (ADR
/// 0015): they live in the keyring or encrypted, and what travels here is
/// the URL exactly as written in the file. An inline `password` is
/// something the CLI itself refuses, and this list does not invent or
/// resolve it.
///
/// **Does not connect.** It returns where one could go; going is navigating
/// to that URL, and that already establishes the session the usual way —
/// with its TOFU, its policy and its degradation notice. A method that
/// "connected" would be a second door to what `fs.list` already does.
///
/// ONLY a `User` connection, for the same reason as [`HOST_VOLUMES`]: the
/// list names the human's servers, and a path scope does not need it.
pub const CONNECTION_LIST: &str = "connection.list";

/// Byte limit returned by ONE call to [`FS_READ`] (before
/// base64). Asking for more is not an error: it gets trimmed and `eof`
/// accounts for it.
pub const FS_READ_MAX_CHUNK: u64 = 8 * 1024 * 1024;

/// Limit of entries returned by ONE [`FS_LIST`] page (0.8.0, ADR
/// 0017). Requesting a larger `limit` is not an error: it gets trimmed to
/// this ceiling (same pattern as [`FS_READ_MAX_CHUNK`]), and the rest
/// follows via `next_cursor`.
pub const FS_LIST_MAX_PAGE: u32 = 10_000;

/// Byte limit of [`PluginHelpResult::markdown`] (H3e, 0.34.0), applied
/// both to the source file and to the decoded TEXT.
///
/// It is here, and not only on the host, because it is NORMATIVE: the
/// contract invites a receiver to size against it, and a non-Rust peer
/// cannot resolve `norte_help::Limits::untrusted()`. The host MUST trim to
/// this number — `norte-core` has a test that anchors the two values, so
/// they cannot drift apart silently — and changing it changes the wire
/// contract, with a bump.
pub const PLUGIN_HELP_MAX_BYTES: usize = 64 * 1024;

/// Is `version` at least `major.minor`?
///
/// For deciding whether the OTHER end knows a specific capability, which is
/// a different question from [`version_compatible`]: that one says whether
/// they can talk at all, this one says whether it is worth asking for
/// something that arrived in a given version. A `false` is not an error —
/// it is the signal to degrade AND SAY SO, which is what separates "this
/// did not happen" from silence.
///
/// A version that does not parse answers `false`: not knowing what the
/// other one speaks, nothing is assumed about it.
///
/// ```
/// use norte_proto::methods::version_at_least;
/// assert!(version_at_least("0.46.0", 0, 46));
/// assert!(version_at_least("0.47.1", 0, 46));
/// assert!(!version_at_least("0.45.9", 0, 46));
/// assert!(!version_at_least("no-semver", 0, 46));
/// ```
#[must_use]
pub fn version_at_least(version: &str, major: u64, minor: u64) -> bool {
    let mut it = version.split('.');
    let (Some(j), Some(n)) = (it.next(), it.next()) else {
        return false;
    };
    let (Ok(j), Ok(n)) = (j.parse::<u64>(), n.parse::<u64>()) else {
        return false;
    };
    (j, n) >= (major, minor)
}

/// Does a `server` core accept a `client`? N and N-1 (spec §11): same
/// major; in 0.x the "effective major" is the minor — the same minor or
/// the immediately preceding one is accepted. The patch never matters.
///
/// ```
/// use norte_proto::methods::version_compatible;
/// assert!(version_compatible("0.4.0", "0.4.9"));
/// assert!(version_compatible("0.4.0", "0.3.0"));
/// assert!(!version_compatible("0.4.0", "0.2.0"));
/// assert!(!version_compatible("0.4.0", "0.5.0")); // cliente del futuro
/// assert!(!version_compatible("0.4.0", "no-semver"));
/// ```
#[must_use]
pub fn version_compatible(server: &str, client: &str) -> bool {
    fn digits(seg: &str) -> Option<u64> {
        // Strict: digits only, no `+`/spaces (which u64::parse tolerates)
        // nor leading zeros (semver forbids them). Pre-release and
        // build metadata are also excluded — deliberate and pinned in
        // tests: a development daemon with a weird tag does NOT negotiate.
        if seg.is_empty() || !seg.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if seg.len() > 1 && seg.starts_with('0') {
            return None;
        }
        seg.parse().ok()
    }
    fn parse(v: &str) -> Option<(u64, u64)> {
        let mut it = v.split('.');
        let major = digits(it.next()?)?;
        let minor = digits(it.next()?)?;
        // The patch must exist and be numeric (semver), but is not compared.
        let _ = digits(it.next()?)?;
        if it.next().is_some() {
            return None;
        }
        Some((major, minor))
    }
    let (Some((sj, sn)), Some((cj, cn))) = (parse(server), parse(client)) else {
        return false;
    };
    if sj != cj {
        return false;
    }
    if sj > 0 {
        // Real stability: same major is enough; N/N-1 applies to the
        // server's minor against newer clients.
        return cn <= sn;
    }
    // 0.x: the minor is the "effective major" — N or N-1.
    cn == sn || cn + 1 == sn
}

/// `fs.list` — list a directory.
pub const FS_LIST: &str = "fs.list";
/// `fs.stat` — metadata of a node.
pub const FS_STAT: &str = "fs.stat";
/// `fs.copy` — copy (recursive if a dir) as a Task.
pub const FS_COPY: &str = "fs.copy";
/// `fs.move` — move as a Task (atomic rename if the provider can).
pub const FS_MOVE: &str = "fs.move";
/// `fs.delete` — delete as a Task: trash (default) or permanent
/// (post-order recursive) — ADR 0009.
pub const FS_DELETE: &str = "fs.delete";
/// `fs.mkdir` — creates ONE directory as a Task (#104, F7). It is NOT
/// `mkdir -p`: the parent must exist (`NotFound` if not), and a prior node
/// at the destination is `Conflict` — creating is an assertion about a FREE
/// name. The `ConflictKind` is the DESTINATION provider's: `Exists` for a
/// byte-exact node (a prior dir included — no silent idempotency),
/// `CaseCollision`/`Normalization` if the destination filesystem collapses
/// the name onto an existing one (macOS/Windows pitfall: evaluated against
/// the destination, not the source). No `on_collision`: creating offers no
/// collision policies. Journal `Created` with undo (rule 4); gated by
/// `PolicyOp::Mkdir`.
pub const FS_MKDIR: &str = "fs.mkdir";
/// `fs.create` — creates ONE EMPTY file as a Task (#290).
///
/// The same as [`FS_MKDIR`] with the other node class, and with its same
/// rules: the parent must exist, a prior node at the destination is
/// `Conflict` — creating is an assertion about a FREE name — and no
/// `on_collision`, because creating offers no collision policies. There is
/// no reading of "create" that means "empty out whatever is there", and a
/// method that silently truncates is data loss with an innocent name.
///
/// **Where that exclusivity comes from, and how far it reaches.**
/// `fs.mkdir` is atomic everywhere because `mkdir(2)` fails with `EEXIST`;
/// creating a file is open-and-confirm in TWO steps, so atomicity is not a
/// theorem of the core: each provider supplies it separately. The local one
/// publishes with an atomic non-replace rename, the object one with
/// `If-None-Match`. **SFTP cannot**: v3 has no atomic rename, so a window
/// remains there between the check and the publish. A client that needs the
/// strong guarantee has to look at the scheme; the rest can treat `Conflict`
/// as definitive.
///
/// Empty and nothing else: writing content is `fs.copy` from somewhere, or
/// the program that opens it afterward. Journal `Created` with undo (rule
/// 4); gated by `PolicyOp::Create`.
pub const FS_CREATE: &str = "fs.create";
/// `fs.set_mode` — changes the POSIX permissions of N paths as a Task
/// (0.60.0, #314, ADR 0081).
///
/// The usual twelve bits — `rwx` for owner, group and others, plus setuid,
/// setgid and sticky — exactly as `chmod(2)` takes them. Nothing else: the
/// rest of `st_mode` says what CLASS the node is, and that is not changed,
/// it just is.
///
/// **It is a mutation**, with everything that carries (rule 4): journal
/// with its reversal — the previous mode, read BEFORE writing the new one —
/// and its own policy gate, `PolicyOp::SetMode`. Separate from `Create` and
/// `Mkdir` for the same reason those two are separate from each other:
/// letting something create files is not letting it change who can read
/// them.
///
/// **No recursion in this round.** Applying to a tree is a different
/// question — what mode a directory gets when what you asked for is a file
/// mode, and what happens to what fails halfway — and answering it halfway
/// would be worse than not answering it. The paths that get sent are the
/// ones that change.
///
/// A provider without POSIX permissions answers `Unsupported` and changes
/// nothing: there is nothing to change inside a `.zip`, and an S3 bucket has
/// no mode. It is advertised with
/// [`CapabilityFlags::POSIX_MODE`](crate::CapabilityFlags), so a frontend
/// can turn off the gesture instead of offering it and failing.
///
/// Limit: [`FS_SET_MODE_MAX_PATHS`] paths, and it is REJECTED instead of
/// trimmed, for the same reason as its siblings — a half batch over
/// PERMISSIONS leaves half the selection with the old ones and does not
/// even say which.
pub const FS_SET_MODE: &str = "fs.set_mode";
/// Path limit of an [`FS_SET_MODE`]: above it, it is REJECTED
/// (`InvalidPath`), not trimmed.
///
/// The same number as the other batches, and for the same reason: what is
/// trimmed silently reads as done.
pub const FS_SET_MODE_MAX_PATHS: usize = 4096;
/// `fs.search` — live search under a subtree (spec §17.1a): name by glob OR
/// regex, content by literal OR regex. Returns a Task
/// (`TaskKind::Search`); hits arrive via the [`SEARCH_HITS`] notification
/// ONLY to the connection that launched it. Cancelable with
/// `task.cancel`. At least one criterion; glob and regex are MUTUALLY
/// EXCLUSIVE per axis.
///
/// Result = the EXISTING [`FsTaskResult`] (`{task_id}`), like
/// `fs.copy`/`fs.move`/`fs.delete` — zero new struct for the result.
///
/// [`TaskProgress`](crate::TaskProgress) mapping during the search
/// (implemented by `norte_core::search::run_walk`):
/// - `entries_done` = entries SCANNED by the walker (including those
///   skipped by a `list`/`read` error), NOT the hits.
/// - `bytes_done` = number of accumulated HITS: a search does not move
///   bytes, so the field is reused as a result counter (the frontend paints
///   it as "N hits").
/// - `entries_total`/`bytes_total` = `None` (a walk does not know its size
///   ahead of time); `current` = last entry seen.
///
/// `max_hits` reached ⇒ the Task ends `Completed` (never `Failed`); the
/// client infers "truncated" by comparing the total hits received with
/// `max_hits`.
pub const FS_SEARCH: &str = "fs.search";
/// Builds/updates a subtree's search index (M4, ADR 0034). It is a Task
/// (progress + cancellation, like [`FS_COPY`]); its result on completion is
/// [`IndexBuildResult`].
pub const INDEX_BUILD: &str = "index.build";
/// Queries a root's index by text (M4, ADR 0034). DIRECT response (not a
/// Task): [`IndexQueryResult`].
pub const INDEX_QUERY: &str = "index.query";
/// `index.embed` — Task ([`TaskKind::Embed`](crate::TaskKind), 0.33.0):
/// generates embeddings of a root's already-indexed files via the
/// configured AI provider (`[ai] embed_provider`). Requires a prior
/// `index.build` (`NotFound` if the root has no rows). Full AI gate; HUMAN
/// connection ONLY (an agent one gets `PolicyDenied`): content prefixes
/// leave the process.
pub const INDEX_EMBED: &str = "index.embed";
/// `index.search_semantic` — direct request (0.33.0), cancelable with
/// `rpc.cancel`: an embed of the query + a cosine sweep in the core. `k` is
/// trimmed to [`INDEX_SEMANTIC_MAX_K`]. HUMAN connection ONLY, like
/// [`INDEX_EMBED`] — the query goes out to the provider.
/// The query is capped server-side (4 KiB) — excess ⇒ `INVALID_PARAMS`.
pub const INDEX_SEARCH_SEMANTIC: &str = "index.search_semantic";
/// Limit of `k` in [`INDEX_SEARCH_SEMANTIC`]. Asking for more is not an
/// error: it gets trimmed (same pattern as [`FS_LIST_MAX_PAGE`]).
pub const INDEX_SEMANTIC_MAX_K: u32 = 100;
/// Suggests a REVIEWABLE rename plan for `dir` (M4-IA, ADR 0031). DIRECT
/// response (not a Task) but CANCELABLE with `rpc.cancel` (#72): the call to
/// the AI provider can take seconds. Mutates NOTHING — applying the plan is
/// N ordinary [`FS_MOVE`]s (journal + undo + policy).
/// [`AiRenamePlanParams`] → [`AiRenamePlanResult`].
pub const AI_RENAME_PLAN: &str = "ai.rename_plan";
/// `fs.rename_batch_plan` — the REVIEWABLE plan for a batch of renames
/// inside ONE directory (0.36.0). DIRECT response: no Task, no journal, no
/// mutation. The client sends INTENT (`pairs`); the core decides order,
/// temporaries and collisions, so a client — which may be an agent — can
/// never slip in an order the human never saw (hard rule 7: logic lives in
/// the core).
/// [`FsRenameBatchPlanParams`] → [`FsRenameBatchPlanResult`].
///
/// "Does not mutate" does NOT mean "harmless". It is a directory READ in
/// disguise: the `External` and `AbsentSource` verdicts say which names
/// exist and which do not, so this method is an existence ORACLE and is
/// subject to the SAME read gate as [`FS_LIST`]/[`FS_STAT`] (#80) — an
/// agent without scope over `dir` gets `PolicyDenied`, not a plan. Whoever
/// implements the dispatch cannot read "no Task, no journal, no mutation"
/// and conclude the opposite.
pub const FS_RENAME_BATCH_PLAN: &str = "fs.rename_batch_plan";
/// `fs.rename_batch` — executes a batch of renames as ONE Task
/// ([`TaskKind::RenameBatch`](crate::TaskKind)) and ONE undoable journal
/// unit (0.36.0). Carries the `plan_hash` of the plan that was reviewed: the
/// core re-plans and refuses with
/// [`Error::PlanStale`](crate::Error::PlanStale) if the directory moved
/// between preview and execution, and with
/// [`Error::PlanNotExecutable`](crate::Error::PlanNotExecutable) if the plan
/// has collisions. Result = the existing [`FsTaskResult`] (`{task_id}`),
/// like `fs.copy`/`fs.move`/`fs.delete`.
///
/// The hash is a FRESHNESS token, not proof of approval: it is a public,
/// deterministic function of `(dir, steps, verdicts)`, with no secret or
/// server-side state, so a client can compute it without ever having called
/// [`FS_RENAME_BATCH_PLAN`]. What it guarantees is that the plan executed is
/// the one the re-plan produces NOW, and — by its binding to the directory
/// — that a hash approved for one directory is not valid against another.
/// Whoever decides whether this call may happen is policy.
pub const FS_RENAME_BATCH: &str = "fs.rename_batch";
/// `fs.rename_batch_report` — the report of an [`FS_RENAME_BATCH`] Task
/// (0.36.0): what got applied, what got undone, and — the one thing that
/// does not fit in an error — WHAT WAS LEFT HALFWAY and under what names.
/// [`FsRenameBatchReportParams`] → [`FsRenameBatchReportResult`].
///
/// It exists for the same reason as [`POLICY_UNDO_REPORT`] (#71): a
/// `Failed` in `task.progress` counts the CAUSE, and a batch whose rollback
/// got stuck leaves the human with a half-renamed directory that has to be
/// NAMEABLE. Without this method the executor keeps its promise ("never a
/// bare error") only for the embedded caller, which gets the report handed
/// to it.
///
/// Snapshot: definitive once the Task is terminal. The server retains
/// reports in a bounded ring, so an id too old — or that was never a batch
/// — is [`Error::NotFound`](crate::Error::NotFound). It is visible to
/// whoever could see the Task: its owner, or any human connection; for
/// everyone else the response is THE SAME as for an unknown id (existence
/// is not leaked, same criterion as `task.cancel`). That an id evicted from
/// the ring is indistinguishable from one that never existed is deliberate:
/// separating them would force separating the third case too, which is
/// exactly the one that cannot be separated (ADR 0042 §8).
pub const FS_RENAME_BATCH_REPORT: &str = "fs.rename_batch_report";

/// Limit of pairs in ONE request to [`FS_RENAME_BATCH_PLAN`] or
/// [`FS_RENAME_BATCH`] (0.36.0). Unlike [`FS_LIST_MAX_PAGE`] it is NOT
/// trimmed: trimming a batch of renames would execute a different plan from
/// the one requested, so the daemon REJECTS the whole request with
/// [`Error::InvalidPath`](crate::Error::InvalidPath) — the same category the
/// embedded API answers for the same excess, so a client does not have to
/// learn two answers depending on which door it comes through.
///
/// 4096 because a human batch — a TV season, a roll of photos — lives two
/// orders of magnitude below it, and because the planner is superlinear
/// over the directory listing and `fs.rename_batch_plan` is a DIRECT
/// response: the work happens on the request's own path, not in a
/// cancelable Task, and `fs.*` is reachable by an agent. The ceiling is the
/// difference between a large batch and a request that occupies the
/// dispatch thread.
///
/// `usize` and not `u32` like [`FS_LIST_MAX_PAGE`]: those bound an integer
/// wire FIELD (`limit`, `k`), this one is only measured against
/// `pairs.len()` — same criterion as [`SEARCH_HITS_MAX_BATCH`] and
/// [`PLUGIN_HELP_MAX_BYTES`], which also bound collection sizes. A `u32`
/// here would only buy an `as usize` in the daemon, in the engine and in the
/// planner.
pub const FS_RENAME_BATCH_MAX_PAIRS: usize = 4096;

/// Path limit of ONE [`FS_CHECKSUM`] request (0.59.0, #311).
///
/// The same number and the same criterion as [`FS_RENAME_BATCH_MAX_PAIRS`]:
/// above it, it is REJECTED, not trimmed. A checksum report silently
/// trimmed is worse than none — it reads as "everything checked" over files
/// nobody looked at, and checking is exactly what this method exists for.
pub const FS_CHECKSUM_MAX_PATHS: usize = 4096;

/// EXACT length of `plan_hash` (0.36.0): sha256 in lowercase hex, 64
/// characters. A hash of another shape is a PARAMS error (`-32602`), never
/// [`Error::PlanStale`](crate::Error::PlanStale) — telling "the directory
/// changed" to whoever sent garbage is lying to them about the world.
pub const PLAN_HASH_LEN: usize = 64;
/// `search.hits` — server→client notification with a BATCH of
/// [`FS_SEARCH`] results. It ONLY travels to the connection that launched
/// the search (never broadcast, same directional criterion as
/// [`POLICY_APPROVAL_REQUIRED`]).
///
/// A client that does not drain its queue loses the frames that do not fit,
/// but NOT its subscription (#155): the owner of a live directed feed keeps
/// its place to receive the Task's terminal snapshot. With `max_hits` set,
/// that snapshot is also what a truncated search is measured against; with
/// `max_hits: None` it is the only signal there is, same as in
/// [`COMPARE_ROWS`].
pub const SEARCH_HITS: &str = "search.hits";
/// Limit of entries per [`SEARCH_HITS`] notification (server-side
/// coalescing, same spirit as [`FS_LIST_MAX_PAGE`]).
pub const SEARCH_HITS_MAX_BATCH: usize = 256;

/// `fs.compare` — compares TWO directory trees and emits one row per pair
/// (0.39.0, ADR 0048). Returns a Task; rows arrive via [`COMPARE_ROWS`]
/// ONLY to the connection that launched it, same as
/// [`FS_SEARCH`]/[`SEARCH_HITS`]. Cancelable with `task.cancel`.
///
/// Result = the EXISTING [`FsTaskResult`] (`{task_id}`), like
/// `fs.copy`/`fs.move`/`fs.search` — zero new struct for the result.
///
/// Mutates NOTHING: no journal, no undo, not a byte written (hard rule 4
/// does not apply, and saying so here stops someone asking for an entry
/// that would mean nothing). What it does do is READ two whole trees, and
/// with the hash rung read their CONTENT — more than a listing reveals —
/// so it is subject to the read gate over BOTH roots, and the hash also
/// requires content scope — which today means a live scope over the root
/// that grants `copy` or `move`, the two ops that do not run without
/// reading bytes. The denial is the usual coarse category (`out-of-scope`),
/// the same as for a root outside scope: if the remedy is to ask for
/// `copy`, this line says so, not the error. Two roots that resolve to the
/// same provider and path are `-32602`: comparing something against itself
/// for an hour is not a request, it is a caller's bug.
///
/// ERRORS are rows, not the end of the Task
/// ([`CompareVerdict::Error`]): an unreadable subdirectory, a directory
/// over [`COMPARE_MAX_DIR_ENTRIES`], or a read that fails mid-hash cost
/// THEIR row and the walk keeps going. A three-hour comparison cannot die
/// on an `EACCES` from leaf 40,000.
pub const FS_COMPARE: &str = "fs.compare";

/// `fs.dir_size` — how much a directory actually takes up (0.49.0, #139).
///
/// A listing tells a file's size and NOT a folder's: knowing it requires
/// walking it entirely, and a listing that did that for every row would
/// turn going down one level into a storm of requests. That is why it is
/// asked for, not assumed.
///
/// Returns a Task, and the TOTAL travels in the progress that already
/// exists: `bytes_done` sums the sizes and `entries_done` counts the
/// entries, so the last snapshot IS the result. Zero new types for the
/// result — [`FsTaskResult`], like `fs.copy` — and zero new notifications:
/// a client that already paints a copy's progress bar knows how to paint
/// this one.
///
/// `bytes_total`/`entries_total` ALWAYS go to `None`: how much it was will
/// be known when it finishes, and faking a total while counting would be a
/// bar advancing toward a made-up number.
///
/// Mutates NOTHING: no journal, no undo, not a byte written (rule 4 does
/// not apply). It reads the tree's SHAPE, not its content, so it is subject
/// to the same read gate as a listing over each root passed to it.
///
/// An unreadable subdirectory costs its own share and the walk continues:
/// counting a three-hour folder cannot die on an `EACCES` from leaf 40,000,
/// and the number that comes out is that of what could be read. **Today the
/// progress has no way to say the number is a FLOOR** — how many branches
/// got skipped does not travel — and that is noted debt, not an oversight.
///
/// Two overlapping roots get REJECTED with
/// [`Error::OverlappingRoots`](crate::Error), as in `fs.compare` and
/// `sync.plan` (#247): `["file:///a", "file:///a/b"]` counted `b` twice and
/// returned a number bigger than what the space actually holds, which is
/// the opposite of what this method exists to answer.
///
/// Sums APPARENT size and not blocks, and does not deduplicate hard links:
/// two names of the same inode count twice. For "does this fit at the
/// destination?" — which is the question — overcounting is the safe side.
pub const FS_DIR_SIZE: &str = "fs.dir_size";
/// `fs.checksum` — the digest of the CONTENT of each path passed to it
/// (0.59.0, #311).
///
/// Returns a cancelable Task ([`FsTaskResult`],
/// [`TaskKind::Checksum`](crate::TaskKind)), and the digests are collected
/// afterward with [`FS_CHECKSUM_REPORT`]. That split is not ceremony: **N
/// digests do not fit in a Task's outcome**, just as
/// [`ARCHIVE_PACK_REPORT`]'s names did not fit, nor
/// [`FS_RENAME_BATCH_REPORT`]'s stuck rename, and progress only knows how to
/// count.
///
/// Mutates NOTHING: no journal, no undo, not a byte written (rule 4 does not
/// apply). It reads CONTENT — not shape, like `fs.dir_size` — so the daemon
/// gates it through the NARROW door, the content one, in addition to the
/// read one (ADR 0080): this subsumes `fs.compare`'s oracle with the hash
/// rung, and with more reach, because a digest is later compared against a
/// dictionary without ever having to place the candidate anywhere.
///
/// **An unreadable file does not kill the batch.** It comes out in the
/// report with its reason and no digest, and the rest still get computed:
/// checking a hundred files cannot die on the one somebody just moved.
/// Cancellation does stop it: that is an order, not a stumble.
///
/// **A DIRECTORY is not walked**: it comes out marked as omitted. Hashing a
/// tree is a different question — a manifest, with its own format and order
/// — and half-answering it here would give a digest that means nothing
/// checkable.
///
/// Limit: [`FS_CHECKSUM_MAX_PATHS`] paths, and it is REJECTED instead of
/// trimmed (same criterion as [`FS_RENAME_BATCH_MAX_PAIRS`]): a report
/// silently trimmed reads as "all fine" over what nobody looked at.
pub const FS_CHECKSUM: &str = "fs.checksum";
/// `fs.checksum_report` — the digests an [`FS_CHECKSUM`] Task computed
/// (0.59.0, #311).
///
/// A twin of [`FS_RENAME_BATCH_REPORT`] and [`ARCHIVE_PACK_REPORT`], and for
/// the same reason as both: there is a true fact about what was just read
/// that does not fit in the Task's outcome.
///
/// It is a SNAPSHOT: definitive once the Task is terminal, partial before —
/// which is exactly what makes asking for it while it runs useful.
pub const FS_CHECKSUM_REPORT: &str = "fs.checksum_report";
/// `fs.dir_usage` — what each CHILD of a directory takes up (0.75.0, phase
/// 4).
///
/// Returns a cancelable Task ([`FsTaskResult`],
/// [`TaskKind::DirUsage`](crate::TaskKind)), and the children get collected
/// afterward with [`FS_DIR_USAGE_REPORT`].
///
/// **Why two methods and not one**, again: the result is a LIST, and that
/// does not fit in a Task's outcome nor in its progress, which only knows
/// how to count. It is the same split — and for the same reason — as
/// [`FS_CHECKSUM_REPORT`], [`FS_RENAME_BATCH_REPORT`] and
/// [`ARCHIVE_PACK_REPORT`].
///
/// **Why it is not a parameter of [`FS_DIR_SIZE`]**: that one answers ONE
/// number over a selection — "does this fit at the destination?" — and
/// that is why its total can travel in progress without inventing any type.
/// This one answers "what is this made of?", which is a different question,
/// with a different shape and a different Task class.
///
/// Mutates NOTHING: no journal, no undo, not a byte written (rule 4 does
/// not apply). It reads the tree's SHAPE and not its content, like
/// `fs.dir_size`, so the daemon gates it through the READ door and not the
/// narrow one.
pub const FS_DIR_USAGE: &str = "fs.dir_usage";
/// `fs.dir_usage_report` — the children an already-launched
/// [`FS_DIR_USAGE`] measured (0.75.0, phase 4).
///
/// It is a SNAPSHOT: partial while the Task runs — which is what makes
/// asking for it useful — and definitive once it is terminal. It is
/// requested by `task_id`, and the daemon checks that whoever asks could
/// SEE that Task: an agent does not read the map another one measured.
///
/// A `task_id` that was never an `fs.dir_usage` from this instance, or
/// whose report the ring already evicted, answers like its twins do: no
/// report. A daemon running for months cannot keep the map of every
/// directory someone looked at.
pub const FS_DIR_USAGE_REPORT: &str = "fs.dir_usage_report";

/// How many children fit in an [`FsDirUsageReportResult`].
///
/// It exists because `children` would be the ONLY unbounded list in the
/// protocol, and the decoder does not degrade: above `MAX_FRAME_BYTES` the
/// frame is not trimmed, it fails entirely. A `/nix/store`, a Maildir or a
/// `node_modules` are hundreds of thousands of top-level children.
///
/// Above the limit, the BIGGEST ones travel — which is what a map paints —
/// and the rest is counted in [`FsDirUsageReportResult::omitted`], never
/// silently: a report trimmed without saying so reads as the whole
/// directory, which is the failure this protocol already forbids itself in
/// `fs.checksum`.
pub const DIR_USAGE_MAX_CHILDREN: usize = 4096;

/// How deep an [`FsDirUsageParams`] can ask to descend.
///
/// Today the server only serves `1`, and that is SAID in the type: a depth
/// that is accepted and ignored is a client that asks for two levels,
/// receives one, and believes it has two.
pub const DIR_USAGE_MAX_DEPTH: u32 = 8;
/// `archive.pack` — manufactures a NEW archive from a set of paths (0.50.0,
/// #132).
///
/// Returns a cancelable Task ([`FsTaskResult`],
/// [`TaskKind::Pack`](crate::TaskKind)), and **does not write inside any
/// container**: the archive provider is still `READ_ONLY` (ADR 0018). What
/// it does is read the entries through their provider and write ONE file
/// through the destination's provider, which can be any other one.
///
/// It MUTATES, so it goes to the journal (rule 4) as ONE creation: undoing
/// it is deleting the archive, and that is a complete undo.
///
/// The [`ArchiveFormat`] travels EXPLICIT. The frontend infers it from the
/// name the user types and shows it before sending; inferring it here would
/// be deciding for them without telling them, and two clients with two
/// heuristics would give two different archives from the same request.
pub const ARCHIVE_PACK: &str = "archive.pack";
/// `archive.pack_report` — what that packing stored that does not survive
/// leaving here (0.58.0, #250).
///
/// [`ArchivePackReportParams`] → [`ArchivePackReportResult`]. Twin of
/// [`FS_RENAME_BATCH_REPORT`] and [`POLICY_UNDO_REPORT`], and for the same
/// reason as both: the Task's outcome speaks to whether the work went
/// through, and this speaks to what was left WRITTEN. A `Completed` is true
/// and the archive can still carry two entries that on macOS are just one.
///
/// It is requested AFTER the Task is terminal, and until then what it
/// returns is the half report — same as the batch's. The server retains
/// them in a bounded ring, so an id too old, or that was never a packing,
/// is [`Error::NotFound`](crate::Error::NotFound); that an evicted id is
/// indistinguishable from one that never existed is deliberate, same
/// criterion as `fs.rename_batch_report`.
///
/// **An empty report is an assertion**, not a silence: it means both things
/// were checked and there was neither. What cannot be done is distinguish
/// it from a 0.57 daemon, which does not know the method — that is why the
/// warning belongs to the client asking, not to the one that cannot.
pub const ARCHIVE_PACK_REPORT: &str = "archive.pack_report";
/// `archive.test` — checks what the format promises for each entry (0.50.0,
/// #132).
///
/// Cancelable Task ([`TaskKind::TestArchive`](crate::TaskKind)) that returns
/// [`ArchiveTestResult`] on completion. Mutates NOTHING: no journal, no
/// undo, not a byte written.
///
/// What gets checked depends on the format and **the result says so**
/// ([`ArchiveTestResult::checked`]): a zip has a CRC-32 per entry and a
/// `tar.gz` a CRC in its tail, but a plain tar has no content checksum at
/// all, so saying "passes" on a plain tar would be asserting more than the
/// format can support.
pub const ARCHIVE_TEST: &str = "archive.test";
/// `file.split` — splits a file into numbered chunks (0.50.0, #132).
///
/// Cancelable Task ([`TaskKind::Split`](crate::TaskKind)), journaled with
/// one creation per chunk. The naming convention is Total Commander's —
/// `name.001`, `name.002`…— which is what the users of the keys that ask
/// for this already have.
pub const FILE_SPLIT: &str = "file.split";
/// `file.combine` — joins the chunks of a [`FILE_SPLIT`] back together
/// (0.50.0, #132).
///
/// Cancelable Task ([`TaskKind::Combine`](crate::TaskKind)), journaled as a
/// creation. It is given the FIRST chunk and finds the rest by convention.
/// A gap in the numbering, or an intermediate chunk of a different size
/// than the first, is [`Error::Conflict`](crate::Error) and not a short
/// file: a badly joined file is a corrupt file that looks fine.
pub const FILE_COMBINE: &str = "file.combine";
/// `archive.test_report` — the report of an already-launched
/// [`ARCHIVE_TEST`] (0.50.0, #132).
///
/// It exists for the same reason as [`FS_RENAME_BATCH_REPORT`]: a Task does
/// not return a value, and what this method has to report — which entry is
/// corrupt and why — does not fit in a Task's `Failed`. It is requested with
/// the `task_id`, and only whoever could see that Task sees it: a foreign
/// id answers the same as one that does not exist.
pub const ARCHIVE_TEST_REPORT: &str = "archive.test_report";
/// The most chunks a [`FILE_SPLIT`] can produce, which is what the `.001`
/// convention allows.
///
/// It is checked BEFORE writing anything: discovering it at chunk 1000
/// would leave a set nobody can join back together.
pub const FILE_SPLIT_MAX_PARTS: u64 = 999;
/// Minimum chunk [`FILE_SPLIT`] accepts, so splitting a file does not
/// produce a million one-byte files.
pub const FILE_SPLIT_MIN_BYTES: u64 = 4096;
/// Failures [`ARCHIVE_TEST`] lists before trimming.
///
/// An archive where EVERYTHING is corrupt cannot cost the client a
/// gigabyte-sized report; the excess is counted in
/// [`ArchiveTestResult::truncated`], the same discipline as
/// [`COMPARE_ROWS_MAX_BATCH`].
pub const ARCHIVE_TEST_MAX_FAILURES: usize = 256;
/// `compare.rows` — server→client notification with a BATCH of
/// [`FS_COMPARE`] rows ([`CompareRowsBatch`]). It ONLY travels to the
/// connection that launched the comparison (never broadcast, same
/// directional criterion as [`SEARCH_HITS`]).
///
/// # How to know whether ALL of them arrived
/// A notification can be lost: a client that does not drain its queue
/// loses the frames that do not fit, and unlike `fs.search` there is no
/// `max_hits` to count against here (MINOR finding from protocol-guardian,
/// C1 review). What it does NOT lose is the subscription: the owner of a
/// live directed feed stays on the map even if its queue fills up,
/// precisely so it receives the terminal snapshot this check is made
/// against (#155 — before, it got evicted, and the check got lost in
/// exactly the case it exists for). The signal is
/// [`TaskProgress::entries_done`](crate::TaskProgress::entries_done), which
/// on a [`TaskKind::Compare`](crate::TaskKind::Compare) Task counts ROWS
/// emitted: `task.progress`'s last snapshot always carries terminal state
/// and final totals, so a client compares what it received against that
/// number and knows whether it is missing something. A sync plan (spec 2)
/// that is going to WRITE from these rows has to make that check.
///
/// WHEN to make it: the terminal snapshot can ARRIVE BEFORE the last batch
/// of rows — the row pump and the progress pump are independent tasks
/// writing to the same sink — so comparing the instant the terminal one
/// arrives reports losses that did not happen. The comparison is made once
/// the row stream has run dry.
pub const COMPARE_ROWS: &str = "compare.rows";
/// Limit of rows per [`COMPARE_ROWS`] notification (server-side coalescing,
/// the same number and the same reason as [`SEARCH_HITS_MAX_BATCH`]: a
/// million rows cannot become a million frames).
pub const COMPARE_ROWS_MAX_BATCH: usize = 256;
/// Limit of entries of ONE directory that [`FS_COMPARE`] pairs in memory.
///
/// The pairing is directory against directory — `fs.list` does not
/// guarantee order, so there are no two ordered streams to merge — and that
/// is O(n) in RAM over the wider directory. Above this ceiling the
/// comparison emits a [`CompareVerdict::Error`] row with
/// [`CompareReason::DirTooLarge`] for THAT directory and continues: an
/// oversized directory costs the directory, never an OOM that takes down
/// the other three hours of work.
pub const COMPARE_MAX_DIR_ENTRIES: usize = 200_000;

/// `sync.plan` — plans a ONE-WAY synchronization
/// (`source` → `dest`) as a cancelable Task (0.40.0, ADR 0049). Result = the
/// EXISTING [`FsTaskResult`] (`{task_id}`), like [`FS_COMPARE`]; steps
/// arrive via [`SYNC_STEPS`] ONLY to the connection that launched the plan,
/// and the plan is CLOSED with [`SYNC_PLAN_DONE`].
///
/// Planning does NOT mutate: underneath it is [`FS_COMPARE`]'s comparison
/// with a decision per row, so it is subject to the SAME read gate over
/// BOTH roots, and the hash rung also requires content scope. The one that
/// writes is [`SYNC_APPLY`], and it writes the RETAINED plan — not whatever
/// the client sends again.
///
/// Two roots that OVERLAP — equal, or one inside the other — are
/// [`Error::OverlappingRoots`](crate::Error::OverlappingRoots) and no Task
/// gets created at all. [`FS_COMPARE`] does allow that pair: comparing
/// `/a` against `/a/sub` costs a walk and writes not a byte. Planning writes
/// inside the source itself has no such license.
pub const SYNC_PLAN: &str = "sync.plan";
/// `sync.steps` — server→client notification with a BATCH of [`SYNC_PLAN`]
/// steps ([`SyncStepsBatch`]). It ONLY travels to the connection that
/// launched the plan (never broadcast, same directional criterion as
/// [`COMPARE_ROWS`]).
///
/// How to know whether ALL of them arrived: same as in [`COMPARE_ROWS`],
/// counting against
/// [`TaskProgress::entries_done`](crate::TaskProgress::entries_done) —
/// which on a [`TaskKind::SyncPlan`](crate::TaskKind::SyncPlan) Task counts
/// STEPS emitted — once the stream has run dry. It matters more here than
/// there: a client that approves a plan from which a batch got lost is
/// approving something it has not seen in full. The approval is made
/// against [`SyncPlanDone::counts`], which is the total the core does
/// know.
///
/// **A batch can arrive BEFORE [`SYNC_PLAN`]'s response**, and with it the
/// `task_id` it correlates against: the Task starts inside the dispatch and
/// its notifications go out over the same queue as the response. A client
/// that throws away batches whose `task_id` it does not know yet silently
/// loses steps and afterward gets a [`SYNC_PLAN_DONE`] that looks complete,
/// so they have to be BUFFERED by `task_id` until the response arrives. It
/// is the same shape as [`COMPARE_ROWS`], and it matters more here: there
/// it paints an incomplete diff, here it approves a write.
pub const SYNC_STEPS: &str = "sync.steps";
/// `sync.plan_done` — notification that CLOSES a plan ([`SyncPlanDone`]):
/// its [`PlanHash`], the counts, the blockers and the `executable` verdict.
/// Arrives once per [`SYNC_PLAN`] Task that finishes well, and to the same
/// connection as the batches.
pub const SYNC_PLAN_DONE: &str = "sync.plan_done";
/// `sync.apply` — executes a RETAINED plan ([`SyncApplyParams`] → the
/// existing [`FsTaskResult`]), as a
/// [`TaskKind::Sync`](crate::TaskKind::Sync) Task and ONE undoable journal
/// unit.
///
/// **Carries NOTHING MORE than the hash**, and that is what turns "execute
/// what was approved" into an invariant instead of a promise: there is no
/// second parameter through which another intent could slip in. The plan
/// lives server-side, bound to the CONNECTION that produced it — nobody
/// applies a plan they did not plan — so a hash that does not name a live
/// plan (another connection, an expired TTL, a restarted daemon) is
/// [`Error::PlanStale`](crate::Error::PlanStale). A MALFORMED hash is not:
/// that dies in [`PlanHash`]'s deserialization as a params error, because
/// "this is not a hash" and "the world moved" are different facts.
///
/// A plan with `executable == false` is refused with
/// [`Error::PlanNotExecutable`](crate::Error::PlanNotExecutable) EVEN IF the
/// hash matches.
pub const SYNC_APPLY: &str = "sync.apply";
/// `sync.report` — the report of a [`SYNC_APPLY`] Task
/// ([`SyncReportParams`] → [`SyncReportResult`]): what got done, what
/// failed and which `batch_id` undoes it. A twin of
/// [`FS_RENAME_BATCH_REPORT`], with its same retention and visibility
/// rules.
pub const SYNC_REPORT: &str = "sync.report";

/// Limit of steps per [`SYNC_STEPS`] notification (server-side coalescing):
/// the same number and the same reason as [`COMPARE_ROWS_MAX_BATCH`] — half
/// a million steps cannot become half a million frames.
pub const SYNC_STEPS_MAX_BATCH: usize = 256;
/// How long an approvable plan lives after closing (0.40.0): ten minutes.
///
/// It is the gap between the plan and its execution, and therefore what the
/// executor has to revalidate: before every DESTRUCTIVE step it checks that
/// the destination is still as the plan recorded it. More TTL is more
/// window for the tree to change underneath; less is a human who gets up
/// for coffee and loses the plan. Once expired, the hash is
/// [`Error::PlanStale`](crate::Error::PlanStale).
pub const SYNC_PLAN_TTL_MS: u64 = 600_000;
/// Limit of blockers LISTED in [`SyncPlanDone::blockers`]. Unlike
/// [`SYNC_MAX_INCLUDE`] this one DOES trim — it is a response, not a request
/// — and that is why [`SyncPlanDone::blockers_total`] travels separately and
/// unbounded: a human needs to know there are 40,000 even if only 256 are
/// shown.
pub const SYNC_MAX_BLOCKERS_REPORTED: usize = 256;
/// Limit of failures LISTED in [`SyncReportResult::failures`]. Today it is
/// the same number as [`SYNC_MAX_BLOCKERS_REPORTED`] and it deliberately has
/// its own name: a third party sizing the failure list should not have to
/// read a constant called "blockers", and sharing the name would tie the
/// futures of both together. Like that one, it TRIMS (it is a response),
/// and that is why [`SyncReportResult::failed`] counts unbounded.
pub const SYNC_MAX_FAILURES_REPORTED: usize = SYNC_MAX_BLOCKERS_REPORTED;
/// Limit of paths in [`SyncPlanParams::include`] (0.40.0). Like
/// [`FS_RENAME_BATCH_MAX_PAIRS`] and for the same reason, it is NOT trimmed:
/// exceeding it is a params error (`-32602`). A silently shortened list
/// plans a synchronization the user did not ask for, and the user would
/// approve it believing they saw it in full.
pub const SYNC_MAX_INCLUDE: usize = 4096;

/// `task.cancel` — cooperative cancellation request. The response only
/// confirms receipt; the final state (`cancelled`, or `completed` if the
/// Task won the race) arrives via [`TASK_PROGRESS`].
///
/// Scope by actor: an agent connection only cancels tasks from its own
/// session; over a foreign task the ack is identical to that of an unknown
/// task (existence is not leaked) and the task keeps running. A human
/// connection cancels any of them.
pub const TASK_CANCEL: &str = "task.cancel";
/// `task.pause` — asks a task to STOP at its next checkpoint (0.82.0, ADR
/// 0147). Cooperative like cancellation: the response only confirms
/// receipt, and the `paused` state arrives via [`TASK_PROGRESS`] once the
/// task actually stops. A chunkless copy (server-to-server) stops once the
/// file in progress finishes.
///
/// Same per-actor scope as [`TASK_CANCEL`], and for the same reason: over a
/// foreign, terminal or unknown task the ack is identical and nothing
/// happens.
///
/// While running, only the classes with checkpoints honor it: `copy`,
/// `move` and `delete`. Any other stops if it had not started yet, and if
/// it was already running it keeps going to the end; a client should not
/// offer it.
pub const TASK_PAUSE: &str = "task.pause";
/// `task.resume` — lets a paused task continue (0.82.0, ADR 0147). Over one
/// that is not paused it does nothing. Same scope as [`TASK_PAUSE`].
pub const TASK_RESUME: &str = "task.resume";
/// `task.move` — raises or lowers a task that HAS NOT STARTED YET within the
/// serial queue (0.83.0, ADR 0149). Over one already running, one that is
/// not in the queue, or an unknown one, it does nothing: the ack is the
/// same. Same per-actor scope as [`TASK_CANCEL`].
pub const TASK_MOVE: &str = "task.move";
/// `connection.trust_host_key` — registers an SSH host key in `known_hosts`
/// after the user confirms it (TOFU flow, ADR 0015 D; 0.7.0, phase 6). It
/// is called after an
/// [`Error::HostKeyUnknown`](crate::Error::HostKeyUnknown) and before
/// retrying the connection. Idempotent. HUMAN trust decision: for an agent
/// connection it is `INVALID_REQUEST`, like e.g.
/// `policy.grant_scope`/`decide`/`undo_session`.
pub const CONNECTION_TRUST_HOST_KEY: &str = "connection.trust_host_key";

/// `connection.provide_secret` — delivers the secret a human just typed for
/// a connection (#325, 0.63.0).
///
/// It is called after an [`Error::SecretNeeded`](crate::Error::SecretNeeded)
/// and before retrying the navigation, exactly like
/// `connection.trust_host_key` after a `HostKeyUnknown`. The daemon stores
/// it IN MEMORY for this session; it does not touch disk and dies with the
/// process.
///
/// HUMAN decision, like trusting a host key: for an agent session it is
/// `INVALID_REQUEST`. An agent that could deliver credentials would be an
/// agent that can authenticate on its own, which is exactly what the policy
/// gate exists to prevent.
pub const CONNECTION_PROVIDE_SECRET: &str = "connection.provide_secret";

/// `connection.close` — closes a path's remote session (0.49.0, #140).
///
/// What gets closed is the SESSION cached under `scheme://authority`, along
/// with any composite archive providers hanging off it: without this,
/// "disconnect" disconnected nothing — the pane went somewhere else and the
/// socket stayed open until the session expired on its own.
///
/// Idempotent: closing what was already gone answers `closed: false` and is
/// not an error. A PROCESS scheme — `file://`, and providers a plugin
/// registers — does not close: there is no session to release, and
/// answering yes would be lying about something that stays exactly the
/// same.
///
/// The next operation on that authority reconnects the usual way. Closing
/// forbids nothing: it releases.
pub const CONNECTION_CLOSE: &str = "connection.close";
/// `connection.degraded` — server→client notification (#44): a remote
/// session was established with DEGRADED security (today: FTP with
/// `tls="allow"` fell back to plaintext because the server rejected
/// `AUTH TLS`). It only informs (the user must KNOW the session is in the
/// clear, ADR 0015 F "never silent"); it asks for no decision. Broadcast
/// only to human connections. An N-1 client ignores it (unknown
/// notification, ADR 0004) — degrades to the previous behavior (log-only).
pub const CONNECTION_DEGRADED: &str = "connection.degraded";
/// `connection.failed` — server→client notification (0.64.0, #322): a
/// remote connection could NOT be established, with the cause in a closed
/// vocabulary and the human sentence in `detail`.
///
/// It exists because the error the caller receives is a CATEGORY
/// (`PermissionDenied` almost always) and does not distinguish an empty
/// secret from a wrong key. See [`ConnectionFailed`] for why it travels
/// here and not inside the error.
///
/// Broadcast only to human connections, like [`CONNECTION_DEGRADED`]: it is
/// a sentence to read, and an agent connection does not read. A 0.63 client
/// ignores it (unknown notification, ADR 0004) and stays exactly as it was.
pub const CONNECTION_FAILED: &str = "connection.failed";

/// The CLOSED vocabulary of [`ConnectionFailed::reason`], in one place.
///
/// It exists because the three pieces that have to agree live in three
/// crates — whoever EMITS it (`norte-core`), whoever TRANSLATES it
/// (`norte-frontend`) and the goldens here — and without a common origin
/// each one keeps its own copy of the seven strings. Renaming one in the
/// emitter turned nothing red: the notification kept going out, the
/// frontend stopped recognizing it and every failure started painting as
/// "unknown reason", forever and silently.
///
/// With this, the emitter proves everything of its own is here and the
/// frontend proves it knows how to translate everything here. The list can
/// GROW — it is additive, and whoever receives a value it does not know
/// falls back on `detail` — but a value cannot change name without both
/// tests saying so.
///
/// ```
/// use norte_proto::methods::CONNECTION_FAILURE_REASONS;
/// assert!(CONNECTION_FAILURE_REASONS.contains(&"secret-empty"));
/// ```
pub const CONNECTION_FAILURE_REASONS: &[&str] = &[
    "secret-missing",
    "secret-empty",
    "secret-not-utf8",
    "secret-store",
    "auth-rejected",
    "no-user",
    "agent",
    // 0.84.0 (#370): the RSA key has a modulus below the minimum. This is a
    // key failure that DOES get counted, unlike the others, because the
    // remedy follows from the sentence: get another key. An N-1 client does
    // not know the string and paints it as unknown, which is still better
    // than a bare "permission denied".
    "rsa-too-small",
];
/// `daemon.going_away` — the daemon warns it is leaving BEFORE it stops
/// accepting (0.46.0).
///
/// It exists because a HANDOFF and a STOP are the same event seen from the
/// client — a closed connection — and the correct response is the opposite
/// in each case: come back, or give up. Without this, a client that always
/// reconnected would keep resurrecting a daemon the user just stopped, and
/// one that never reconnected would leave the session dead after an
/// upgrade.
///
/// Goes to ALL connections, agent ones too: an agent's session dies with
/// the daemon the same as a human's, and it needs to know.
pub const DAEMON_GOING_AWAY: &str = "daemon.going_away";
/// `task.progress` — server→client notification, coalesced (≤30 Hz).
pub const TASK_PROGRESS: &str = "task.progress";
/// `policy.request_scope` — an agent asks for a scope (paths+ops+TTL, M3-3b).
pub const POLICY_REQUEST_SCOPE: &str = "policy.request_scope";
/// `policy.grant_scope` — a human grants a pending scope.
pub const POLICY_GRANT_SCOPE: &str = "policy.grant_scope";
/// `policy.decide` — a human approves/denies a pending approval.
pub const POLICY_DECIDE: &str = "policy.decide";
/// `policy.pending` — list of pending approvals (resync).
pub const POLICY_PENDING: &str = "policy.pending";
/// `policy.approval_required` — server→client notification: an `ask` op
/// awaits a decision (M3-3b).
pub const POLICY_APPROVAL_REQUIRED: &str = "policy.approval_required";
/// `policy.undo_session` — undoes an AGENT's entire session (M3-4): a human
/// reverts, in strict LIFO, everything `session` did. User connections
/// only (an agent session does not undo others nor itself through this
/// path). Lives in `policy.*` (the agent-governance family: scopes,
/// approvals, undo) — `session.*` stays reserved for the UI session (spec
/// §11).
pub const POLICY_UNDO_SESSION: &str = "policy.undo_session";
/// `policy.undo_report` — report of an undo Task (0.16.0, #71): the
/// counters of [`POLICY_UNDO_SESSION`] (reverted, skipped and why) and the
/// LIFO's first blocker, if there was one. It is a SNAPSHOT: definitive
/// once the Task is terminal ([`TASK_PROGRESS`]/[`TASK_LIST`]); before that,
/// partial. The server retains the reports of the last undo Tasks (bounded
/// ring, best effort): an unknown or evicted `task_id` is the taxonomy's
/// `Error::NotFound` (since 0.79.0; before, a bare `INVALID_PARAMS`).
/// User connections ONLY (same barrier as the undo that generates it).
pub const POLICY_UNDO_REPORT: &str = "policy.undo_report";
/// `journal.list` — the journal's entries, from newest backward (0.76.0,
/// phase 7 of the WOW program).
///
/// This is the timeline's content: what has been done, who did it and
/// whether it can be reversed. It does not return each entry's hash or the
/// chain — that is what verification is for, which is a different question
/// and a different surface (`norte doctor`) — it returns what a human reads
/// before deciding how far to undo.
///
/// # Who can call it
///
/// A human connection ONLY, with the same criterion and the same error as
/// [`LOG_TAIL`]: `Error::PolicyDenied` with `rule: "not-approved"`, which on
/// the wire is [`codes::APP_ERROR`](crate::wire::codes::APP_ERROR) (`-32000`)
/// and NOT `INVALID_REQUEST`. The reason is even stronger than the log's:
/// the journal is the complete list of everything touched on this machine,
/// with source and destination paths, so for an agent session with a
/// bounded scope it is an existence oracle over EVERYTHING outside its
/// enclosure, and it also shows what the other sessions have done. An
/// empty journal and a forbidden journal cannot read the same.
///
/// # Pagination
///
/// BACKWARD and by `seq`, which is monotonic and never reused: what is
/// requested is what comes before [`JournalListParams::before_seq`] and up
/// to [`JournalListParams::limit`] is received, trimmed by
/// [`JOURNAL_LIST_MAX_PAGE`]. There is no subscription or per-client state,
/// for the same reason [`LOG_TAIL`] is pulled and not pushed.
///
/// ```
/// assert_eq!(norte_proto::methods::JOURNAL_LIST, "journal.list");
/// ```
pub const JOURNAL_LIST: &str = "journal.list";
/// `journal.undo_after` — undoes, in LIFO order, what the HUMAN did after
/// `seq` (0.76.0, phase 7 of the WOW program).
///
/// Returns a Task, like [`POLICY_UNDO_SESSION`], and its report is read via
/// [`POLICY_UNDO_REPORT`] — it is the same report because it is the same
/// undo: the same units (a batch reverts whole or not at all), the same
/// per-unit policy gate, the same strict LIFO that stops at the first
/// blocker, and the same counters for skipped irreversibles.
///
/// The ONLY thing that changes compared to undoing a whole session is which
/// entries go in: the human actor's with a `seq` greater than the given
/// one. It is not "go back to that moment's state" — that does not exist:
/// the irreversible does not come back, and the report counts it — but
/// "undo mine from here on, and stop as soon as something does not add up".
///
/// # Who can call it
///
/// A human connection ONLY, like [`POLICY_UNDO_SESSION`]: this reverts
/// work, and deciding how far is the human's call. An agent session asking
/// for it could erase the trace of what it did.
///
/// ```
/// assert_eq!(norte_proto::methods::JOURNAL_UNDO_AFTER, "journal.undo_after");
/// ```
pub const JOURNAL_UNDO_AFTER: &str = "journal.undo_after";
/// `ai.organize_plan` — a REVIEWABLE plan for reorganizing a directory
/// (0.77.0, phase 8 of the WOW program).
///
/// It is [`AI_RENAME_PLAN`] with one more freedom: each file's destination
/// can carry SUBDIRECTORIES, so the plan also creates folders. That makes
/// it a different operation and not just one more field — a different
/// reversal, a different dialog, a different way to go wrong.
///
/// **Mutates nothing**, like its sibling: the plan is the product, and
/// applying it is [`FS_ORGANIZE`]. It goes through the same AI gate, which
/// is what bounds which names leave the machine.
///
/// ```
/// assert_eq!(norte_proto::methods::AI_ORGANIZE_PLAN, "ai.organize_plan");
/// ```
pub const AI_ORGANIZE_PLAN: &str = "ai.organize_plan";
/// `plugin.organize_plan` — the same plan, proposed by an `organizer`-kind
/// plugin (`norte:organizer@0.1.0`, 0.77.0).
///
/// Same split as `renamer` (ADR 0095): the plugin PROPOSES and the core
/// executes. What makes the operation safe is not where the names came
/// from, so a plugin's plan and a model's plan land in the SAME review and
/// apply through the SAME path.
///
/// ```
/// assert_eq!(
///     norte_proto::methods::PLUGIN_ORGANIZE_PLAN,
///     "plugin.organize_plan"
/// );
/// ```
pub const PLUGIN_ORGANIZE_PLAN: &str = "plugin.organize_plan";
/// `fs.organize` — applies an organizing plan (0.77.0, phase 8).
///
/// **It is a mutation**, with everything that carries (rule 4): it creates
/// the missing directories and moves, ALL under a single `batch_id`, so it
/// undoes as one unit. That is the reason it is one method and not N
/// client calls: loose `fs.create` and `fs.move` would leave a batch that,
/// when undone, returns the files and forgets the folders.
///
/// Carries the `plan_hash` of the plan that was reviewed, like
/// [`FS_RENAME_BATCH`]: what gets applied has to be what a human read.
///
/// ```
/// assert_eq!(norte_proto::methods::FS_ORGANIZE, "fs.organize");
/// ```
pub const FS_ORGANIZE: &str = "fs.organize";
/// `plugin.list` — enumerates DISCOVERED plugins plus load errors (M4-P3).
/// Read-only and OPEN (any connection can query it): a frontend paints the
/// catalogue and the state (approved/enabled) without mutating anything.
pub const PLUGIN_LIST: &str = "plugin.list";
/// `plugin.set_approval` — a HUMAN approves (or revokes) a plugin's
/// capabilities (M4-P3). User connections ONLY: an agent session never
/// grants itself plugin capabilities.
pub const PLUGIN_SET_APPROVAL: &str = "plugin.set_approval";
/// `plugin.set_enabled` — a HUMAN enables or disables a plugin (M4-P3).
/// User connections ONLY (same barrier as [`PLUGIN_SET_APPROVAL`]).
pub const PLUGIN_SET_ENABLED: &str = "plugin.set_enabled";
/// `plugin.uninstall` — a HUMAN uninstalls a plugin (0.71.0, ADR 0104):
/// deletes its directory under `plugins/` and leaves its state entry OFF and
/// UNAPPROVED, so one installed afterward with the same id does not inherit
/// a consent nobody gave it. User connections ONLY (same barrier as
/// [`PLUGIN_SET_APPROVAL`]): withdrawing a consent is as much the human's
/// call as giving it, and deleting configuration files, even more so.
/// Irreversible over the wire: there is no `plugin.install` — installing is
/// still the CLI's job, which is where the source directory is.
pub const PLUGIN_UNINSTALL: &str = "plugin.uninstall";
/// `plugin.run_command` — runs a command of an APPROVED and ENABLED
/// `command` plugin (M4-P4); the plugin runs sandboxed; returns the
/// command's string or an error.
pub const PLUGIN_RUN_COMMAND: &str = "plugin.run_command";
/// `plugin.preview` — runs the first APPROVED and ENABLED previewer plugin
/// that handles the file's mimetype (M4-P5), over the bytes the core reads;
/// all `None` = no previewer applies (the frontend falls back to the raw
/// view).
pub const PLUGIN_PREVIEW: &str = "plugin.preview";
/// `plugin.preview_styled` — STYLED twin of [`PLUGIN_PREVIEW`] (0.27.0, G3,
/// ADR 0037): the same previewer returns lines of spans with `role`/`fg`
/// instead of a plain string, so the HOST paints real highlighting (never
/// the plugin). Same all-or-nothing pattern as [`PLUGIN_PREVIEW`]. A 0.26
/// daemon NEVER sees it: a 0.27 client does not complete the handshake
/// against it (`VERSION_MISMATCH` in `initialize`, see
/// [`version_compatible`]). `MethodNotFound` is the answer within the SAME
/// 0.27 window (a 0.27 daemon without the handler wired yet, T3/T4); the
/// client falls back to [`PLUGIN_PREVIEW`] either way.
pub const PLUGIN_PREVIEW_STYLED: &str = "plugin.preview_styled";
/// `plugin.thumbnail` (0.73.0, ADR 0107): a THUMBNAIL of a file by the
/// first consented `thumbnail` plugin whose mimetype matches. Like
/// `plugin.preview`, the daemon reads the bytes under the same read gate
/// and bounds them; what comes back is an encoded image with its
/// dimensions, verified by the plugin-host before crossing. With no
/// matching plugin, `null`: the window keeps what it had. Open, like its
/// twin.
pub const PLUGIN_THUMBNAIL: &str = "plugin.thumbnail";
/// `plugin.panel_render` (0.74.0, phase 3): the FRAME a `panel` plugin
/// paints in a layout slot.
///
/// The guest does not draw: it describes styled lines and clickable zones
/// that name catalogue commands, so a click of theirs can do nothing the
/// reader could not do with a key. Cosmetic and fail-soft like the preview:
/// with no plugin, no consent or a broken guest, what comes back is `{}` —
/// the result travels with `flatten`, so "nothing" is an empty object and
/// not `null` — and the slot keeps the last frame it had.
pub const PLUGIN_PANEL_RENDER: &str = "plugin.panel_render";
/// `plugin.decorate` — git-status-like decorations per entry, contributed
/// by APPROVED and ENABLED `decorator` plugins (0.27.0, G3, ADR 0037):
/// batched over one visible page, POSITIONAL 1:1 with `params.paths` (see
/// [`PluginDecorateResult`]). A 0.26 daemon NEVER sees it (same handshake
/// `VERSION_MISMATCH` as [`PLUGIN_PREVIEW_STYLED`]); `MethodNotFound` from a
/// 0.27 daemon without the handler wired yet, or the client's decision not
/// to call it, degrade the same way to listing without decorations.
pub const PLUGIN_DECORATE: &str = "plugin.decorate";
/// `plugin.column_values` — values of a column contributed by an APPROVED
/// and ENABLED `columns` plugin (0.27.0, G3, ADR 0037), POSITIONAL 1:1 with
/// `params.paths` (see [`PluginColumnValuesResult`]). Same fallback story as
/// [`PLUGIN_DECORATE`]: a 0.26 daemon never sees it (handshake
/// `VERSION_MISMATCH`); `MethodNotFound`/the client's decision degrade to
/// not showing the column.
pub const PLUGIN_COLUMN_VALUES: &str = "plugin.column_values";
/// `plugin.rename_plan` — the rename plan a `renamer` plugin PROPOSES
/// (0.67.0, C3, ADR 0095) for `dir`'s names:
/// [`PluginRenamePlanParams`] → [`AiRenamePlanResult`], the SAME result as
/// [`AI_RENAME_PLAN`] because it is the same plan from a different producer
/// — it gets reviewed, checked (`fs.rename_batch_plan`) and executed the
/// same way. Mutates nothing. Open to any actor that passes `dir`'s read
/// gate, like `plugin.column_values`; `names` has the limit of
/// [`AI_RENAME_NAMES_MAX`].
///
/// Errors: `NotFound` if the plugin/renamer is not consented; `Io` if the
/// guest does not run. If the guest REFUSES it is not an error: empty
/// `entries` and [`AiRenamePlanResult::refused`] with its sentence (0.68.0,
/// #332), which the daemon masks and bounds before it crosses. A 0.66
/// daemon does not negotiate with this client.
pub const PLUGIN_RENAME_PLAN: &str = "plugin.rename_plan";
/// `plugin.notice` — server→client notification (0.69.0, ADR 0100): a
/// `hook` plugin has something to tell the human about a mutation the
/// journal already recorded, or the daemon shut down the hooks of a plugin
/// that failed three times in a row. See [`PluginNotice`].
///
/// Broadcast only to human connections, like [`CONNECTION_FAILED`]: it is a
/// sentence to read, and an agent connection does not read. A 0.68 client
/// ignores it (unknown notification, ADR 0004): the hook still ran — the
/// source is the journal, not the frontend — only its sentence reached
/// nobody.
pub const PLUGIN_NOTICE: &str = "plugin.notice";

/// The CLOSED vocabulary of [`PluginNotice::kind`], in one place, for the
/// same reason as [`CONNECTION_FAILURE_REASONS`]: the emitter, the
/// translator and the goldens all draw from one list.
///
/// ```
/// use norte_proto::methods::PLUGIN_NOTICE_KINDS;
/// assert!(PLUGIN_NOTICE_KINDS.contains(&"notify"));
/// assert!(PLUGIN_NOTICE_KINDS.contains(&"hooks-disabled"));
/// assert!(PLUGIN_NOTICE_KINDS.contains(&"effect-denied"));
/// ```
pub const PLUGIN_NOTICE_KINDS: &[&str] = &["notify", "hooks-disabled", "effect-denied"];

/// What travels in [`PLUGIN_NOTICE`]: a notice attributed to a plugin.
///
/// `text` is THIRD-PARTY TEXT: the daemon masks it (control chars, bidi)
/// and bounds it before it crosses, and the frontend paints it as a status
/// sentence, attributed to `plugin_id`, and never interprets it. It does
/// not carry the path of the file that prompted it: the hook had it, and if
/// it wanted to name it, it put it in the sentence.
///
/// An unknown `kind` is shown as `text` if there is one: that makes a
/// FUTURE class whose text must not be shown verbatim a breaking change,
/// not an additive one.
///
/// ```
/// use norte_proto::methods::PluginNotice;
/// let n: PluginNotice = serde_json::from_str(
///     r#"{"plugin_id":"org.norte.rename-log","kind":"hooks-disabled"}"#,
/// ).unwrap();
/// assert_eq!(n.text, None);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginNotice {
    /// The plugin it comes from, as listed by `plugin.list`.
    pub plugin_id: String,
    /// What class of notice, closed vocabulary [`PLUGIN_NOTICE_KINDS`]:
    /// `"notify"` is a sentence from the hook (with `text`); `"hooks-disabled"`
    /// is from the daemon — that plugin's hooks got shut down until
    /// reactivating it after three failures in a row — and travels without
    /// `text`; `"effect-denied"` (0.70.0, ADR 0101) is also from the daemon:
    /// the human's policy denied a sidecar the plugin asked to write, said
    /// once per plugin. An unknown value is shown as `text` if there is one
    /// and discarded if not.
    pub kind: String,
    /// The sentence, already masked and bounded by the daemon. Absent for
    /// the daemon's own notices, which the frontend translates by `kind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// `plugin.get_config` — `[config]` schema + EFFECTIVE values of a plugin
/// (0.28.0, G3c, ADR 0037): one [`PluginConfigKeyWire`] element per declared
/// key, schema and CURRENT value together (`schema+value together`) — a
/// remote extension manager had no way to read this before this bump (P2
/// deliberately left it host-only, see the rustdoc of
/// `norte_core::PluginRegistry::settings_of`). An unknown `id` answers with
/// `keys: []` (the same lenient criterion as `plugin.list` with an empty
/// catalogue — never an error for "I have nothing to show"). OPEN to any
/// connection (reading a schema/value consents to nothing, same criterion
/// as `plugin.preview*`).
pub const PLUGIN_GET_CONFIG: &str = "plugin.get_config";
/// `plugin.set_config` — persists ONE `[config]` value for a plugin (0.28.0,
/// G3c, ADR 0037), after validating it against the manifest's SCHEMA (the
/// SAME validation as `config.toml`, never a parallel path — see
/// `norte_plugin_host::encode_wire_value`). An invalid value persists
/// nothing (`INVALID_PARAMS`). Only a HUMAN (non-agent) connection can call
/// it — same criterion as [`PLUGIN_SET_APPROVAL`]/[`PLUGIN_SET_ENABLED`]: a
/// plugin's settings are USER data, an agent does not edit them on its own.
pub const PLUGIN_SET_CONFIG: &str = "plugin.set_config";
/// `plugin.help` — a plugin's help page (H3e, 0.34.0), ON DEMAND: the host
/// returns `help.md` already BOUNDED (byte limit of
/// `norte_help::Limits::untrusted`) and already decoded to valid UTF-8,
/// with two flags counting what happened while bounding it. OPEN like
/// [`PLUGIN_LIST`]: reading documentation consents to nothing.
///
/// The text is THIRD-PARTY and is not masked: the frontend parses it again
/// with `norte_help::parse_untrusted`, which masks while building the
/// model. Parsing on both sides is deliberate — host-side so `norte doctor`
/// and the catalogue can report problems with no frontend around,
/// client-side because the wire carries TEXT, not a tree.
///
/// An UNKNOWN id is `INVALID_PARAMS` (same treatment
/// [`PLUGIN_SET_APPROVAL`] gives a ghost plugin), NOT an empty page. Careful
/// with the analogy: the openness is [`PLUGIN_LIST`]'s, but the leniency
/// with ids is NOT [`PLUGIN_GET_CONFIG`]'s, which answers `keys: []` for an
/// unknown id and never fails. Here it does fail, on purpose: the empty
/// page already MEANS something different ("this plugin exists and
/// documented nothing"), so returning it for a nonexistent plugin too would
/// erase the difference a frontend needs to decide whether its catalogue is
/// stale.
pub const PLUGIN_HELP: &str = "plugin.help";
/// `rpc.cancel` — client→server notification (#72): withdraws the in-flight
/// request whose JSON-RPC `id` it names. Best-effort and WITHOUT a
/// response: the real confirmation is that the cancelled request answers
/// with its outcome ([`Error::Cancelled`](crate::Error::Cancelled) if it was
/// suspended in a policy Ask). It is purely of the RPC LAYER (not
/// `policy.*`): it cancels a request, not an approval (the requester does
/// not know the `approval_id`, which goes to the human). In M3 the only
/// suspendable long path in the dispatch is the Ask; a long op that is
/// already a Task is cancelled with [`TASK_CANCEL`]. An unknown, already
/// resolved, or not-suspended `id` = a benign no-op. An N-1 daemon that
/// does not know it silently discards it (unknown notification, ADR 0004):
/// degrades to the previous behavior (zombie Ask until the TTL), does not
/// break.
pub const RPC_CANCEL: &str = "rpc.cancel";

/// Params of [`FS_LIST`] (cursor pagination since 0.8.0, ADR 0017).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsListParams {
    /// Directory to list. MANDATORY when continuing too (validates that the
    /// `cursor` corresponds to THIS listing).
    pub path: VPath,
    /// Maximum entries for THIS page. `None` (or absent) = no limit (drains
    /// the rest). Trimmed to [`FS_LIST_MAX_PAGE`]; `Some(0)` is an error
    /// (`-32602`, avoids an infinite loop of empty pages). A 0.7 client does
    /// not send it.
    #[serde(default)]
    pub limit: Option<u32>,
    /// OPAQUE cursor for the next page (the previous response's
    /// `next_cursor`). `None` = opens a new listing. Never parsed; unknown
    /// or expired → [`Error::CursorExpired`](crate::Error::CursorExpired).
    #[serde(default)]
    pub cursor: Option<String>,
    /// Ids of the provider attributes to deliver with each entry (0.30.0,
    /// ADR 0039). Empty (the default, and the only possibility for a 0.29
    /// client) = none: nothing is delivered unrequested. The contract is at
    /// most [`ATTRS_MAX_REQUEST`](crate::attrs::ATTRS_MAX_REQUEST) ids, each
    /// well-formed ([`is_valid_attr_id`](crate::attrs::is_valid_attr_id)), and
    /// violating either is `-32602` — a rule BLOCK 2 wires up: a 0.30.0 daemon
    /// ships no producer and ignores the requested ids entirely, so today the
    /// answer to any request is the same empty set of attributes. An id the
    /// provider does not offer is NOT an error either way: it comes back
    /// absent, so a client with a stale catalog degrades instead of failing.
    ///
    /// # Bounded at decode, but NOT validated — on purpose
    ///
    /// This is a REQUEST: data this peer is SENDING, not data it received. So
    /// whatever the wire carries lands in the vector VERBATIM — malformed ids
    /// and over-cap length included. Silently dropping a bad id here would
    /// turn "the client asked for `../etc/passwd`" into "the client asked for
    /// nothing", hiding a caller's bug and making the daemon-side validation
    /// untestable. The asymmetry with the two RECEIVE-side fields —
    /// [`Entry::attrs`](crate::Entry::attrs) and
    /// [`FsCapabilitiesResult::attrs`], which both filter at decode — is
    /// deliberate: a bad cell or a bad advertised descriptor costs itself,
    /// while a bad request is the daemon's `-32602` to raise.
    ///
    /// The one thing decoding does impose is a MEMORY bound, which is not
    /// validation: only the first `ATTRS_MAX_REQUEST + 1` elements are kept
    /// and the rest is drained unmaterialised, because a 16 MiB frame of
    /// `["a","a",…]` would otherwise allocate ~15× its own size in `String`
    /// headers before any daemon check could run. The `+ 1` is what keeps
    /// over-cap OBSERVABLE (`attrs.len() > ATTRS_MAX_REQUEST`) instead of
    /// trimming a violation into legality.
    #[serde(
        default,
        deserialize_with = "crate::attrs::deserialize_attr_request",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(extend(
            "maxItems" = crate::attrs::ATTRS_MAX_REQUEST,
            "items" = serde_json::json!({
                "type": "string",
                "maxLength": crate::attrs::ATTR_ID_MAX,
                "pattern": r"^[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+$",
            })
        ))
    )]
    pub attrs: Vec<String>,
}

/// Result of [`FS_LIST`].
///
/// Compatibility clause (ADR 0004/0017): with neither `cursor` NOR `limit`,
/// the core RETURNS the COMPLETE listing with `next_cursor: null` — a 0.7
/// (N-1) client receives exactly what it got before, never a silent
/// truncation.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsListResult {
    /// Entries of THIS page (order: the provider's, no guarantee).
    pub entries: Vec<Entry>,
    /// Cursor for the next page, or `None` if the listing finished. A 0.7
    /// client ignores it (an unknown field to it).
    #[serde(default)]
    pub next_cursor: Option<String>,
    /// CONTAINER entries omitted from its ENTIRE index (#93, since 0.22.0):
    /// hostile names/anti-bomb limits of an archive provider (ADR 0018 C2).
    /// It is a total PER CONTAINER, not per page or per directory (the
    /// omitted ones have no representable path to attribute to): every page
    /// of the listing repeats the same value. Absent (`None`) = does not
    /// apply or unknown; clients should only flag it when it is `Some(n)`
    /// with `n > 0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<u64>,
    /// The OPAQUE identity of the listed directory (#295, since 0.54.0):
    /// what a client retains to be able to say AFTERWARD, when copying or
    /// moving here, which directory the human was looking at when they
    /// approved.
    ///
    /// It repeats on every page of the same listing, like `skipped` and for
    /// the same reason: a page is not a different directory. Absent
    /// (`None`) = the destination does not know how to give a node identity
    /// (a bucket, an SFTP) or the daemon is 0.53 or older; then the client
    /// does not send an anchor and the write behaves as always.
    ///
    /// A client **never interprets it**: it keeps it as-is and returns it in
    /// [`FsCopyParams::dest_anchor`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir_anchor: Option<crate::entry::DirAnchor>,
}

/// Params of [`FS_STAT`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsStatParams {
    /// Node to query.
    pub path: VPath,
    /// Ids of the provider attributes to deliver with the entry (0.30.0,
    /// ADR 0039). Empty (the default, and the only possibility for a 0.29
    /// client) = none: nothing is delivered unrequested. The contract is at
    /// most [`ATTRS_MAX_REQUEST`](crate::attrs::ATTRS_MAX_REQUEST) ids, each
    /// well-formed ([`is_valid_attr_id`](crate::attrs::is_valid_attr_id)), and
    /// violating either is `-32602` — a rule BLOCK 2 wires up: a 0.30.0 daemon
    /// ignores the requested ids entirely. An id the provider does not offer
    /// is NOT an error either way: it comes back absent.
    ///
    /// # Bounded at decode, but NOT validated — on purpose
    ///
    /// Same as [`FsListParams::attrs`], for the same reasons: a malformed id
    /// survives decoding and is the daemon's `-32602` to raise, instead of
    /// being laundered into "asked for nothing"; only the RECEIVE-side fields
    /// — [`Entry::attrs`](crate::Entry::attrs) and
    /// [`FsCapabilitiesResult::attrs`] — filter. Decoding keeps the first
    /// `ATTRS_MAX_REQUEST + 1` elements and drains the rest, which is a memory
    /// bound (a 16 MiB frame of tiny ids would otherwise allocate ~15× its own
    /// size) that leaves over-cap observable.
    #[serde(
        default,
        deserialize_with = "crate::attrs::deserialize_attr_request",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(extend(
            "maxItems" = crate::attrs::ATTRS_MAX_REQUEST,
            "items" = serde_json::json!({
                "type": "string",
                "maxLength": crate::attrs::ATTR_ID_MAX,
                "pattern": r"^[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+$",
            })
        ))
    )]
    pub attrs: Vec<String>,
}

/// Result of [`FS_STAT`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsStatResult {
    /// The node's metadata.
    pub entry: Entry,
}

/// Params of [`FS_COPY`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsCopyParams {
    /// Source (file or directory).
    pub from: VPath,
    /// EXACT destination (with `RenameAuto` the core derives the free
    /// name; with the rest of the policies it never invents names).
    pub to: VPath,
    /// What to do if the destination exists. `#[serde(default)]`: an N-1
    /// client that does not send it gets `Fail` (the usual behavior).
    #[serde(default)]
    pub on_collision: CollisionPolicy,
    /// What to do with the source's symlinks (default `Preserve`).
    #[serde(default)]
    pub symlinks: SymlinkPolicy,
    /// Resumption (ADR 0012); default `Off` = M1's contract.
    #[serde(default)]
    pub resume: ResumePolicy,
    /// Verification of the partial file on resume; default `Length`.
    #[serde(default)]
    pub verify: VerifyPolicy,
    /// The identity the client observed for `to`'s DIRECTORY when it listed
    /// it (#295, since 0.54.0), exactly as [`FsListResult::dir_anchor`]
    /// gave it.
    ///
    /// Present = "write there only if that directory is still the same node
    /// I was looking at". It is the only thing that distinguishes a
    /// `dest/sub -> /etc` planted before anybody looked from a legitimate
    /// `~/copies -> /mnt/disk/copies`, because both resolve elsewhere and
    /// look the same from the core (ADR 0072).
    ///
    /// Absent (`None`) = 0.53's behavior: it confines the same way, without
    /// that check. A 0.53 client does not send it and loses no correctness,
    /// only the check — and it cannot know it did not happen, the same
    /// nuance ADR 0071 records for `expected_digest` and that applies here
    /// too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_anchor: Option<crate::entry::DirAnchor>,
    /// To the QUEUE instead of in parallel (0.83.0, ADR 0149): queued ones
    /// run one at a time, in arrival order. `#[serde(default)]`: an N-1
    /// client does not send it and its transfer runs in parallel, which is
    /// the usual behavior, and a transfer that does not ask for it travels
    /// byte for byte like in 0.82.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub queued: bool,
}

/// Params of [`FS_MOVE`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsMoveParams {
    /// Source.
    pub from: VPath,
    /// Exact destination (see [`FsCopyParams::to`]).
    pub to: VPath,
    /// What to do if the destination exists (see [`FsCopyParams::on_collision`]).
    #[serde(default)]
    pub on_collision: CollisionPolicy,
    /// What to do with symlinks (only applies to the copy+delete path; a
    /// same-provider rename moves the link as-is).
    #[serde(default)]
    pub symlinks: SymlinkPolicy,
    /// Resumption for the copy+delete path (ADR 0012); default `Off`.
    #[serde(default)]
    pub resume: ResumePolicy,
    /// Verification of the partial file on resume; default `Length`.
    #[serde(default)]
    pub verify: VerifyPolicy,
    /// The observed identity of `to`'s directory (see
    /// [`FsCopyParams::dest_anchor`]; #295, since 0.54.0).
    ///
    /// Applies to BOTH paths of a move. The copy one writes the same as a
    /// copy and also deletes the source afterward; and the rename, even
    /// though it composes no new path under the destination, does resolve
    /// `to` once by path — with the directory turned into a symlink, it
    /// leaves the file on the other side exactly the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_anchor: Option<crate::entry::DirAnchor>,
    /// To the QUEUE instead of in parallel (0.83.0, ADR 0149): queued ones
    /// run one at a time, in arrival order. `#[serde(default)]`: an N-1
    /// client does not send it and its transfer runs in parallel, which is
    /// the usual behavior, and a transfer that does not ask for it travels
    /// byte for byte like in 0.82.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub queued: bool,
}

/// Params of [`FS_DELETE`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsDeleteParams {
    /// Node to delete (recursive if a dir).
    pub path: VPath,
    /// Trash or permanent. `#[serde(default)]` = Trash: the wire's default
    /// is the SAFE one (ADR 0009).
    #[serde(default)]
    pub mode: DeleteMode,
}

/// Params of [`FS_CREATE`] (#290).
///
/// ```
/// use norte_proto::methods::FsCreateParams;
/// let p: FsCreateParams =
///     serde_json::from_str(r#"{"path":"file:///casa/nuevo.txt"}"#).expect("params");
/// assert_eq!(p.path.file_name().expect("name").as_bytes(), b"nuevo.txt");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsCreateParams {
    /// The file to create, COMPLETE. The parent must exist; no path is created.
    pub path: VPath,
    /// The anchor of the directory it is created in (#295, ADR 0073).
    /// Omitted when the caller did not list that directory.
    ///
    /// `fs.mkdir` does not carry it and this method DOES, and the
    /// difference is not a symmetry oversight: `fs.create` is the ONLY wire
    /// method whose success hands a path to a program OUTSIDE norte — a
    /// frontend creates the file to open it with the desktop's editor. With
    /// a symlink planted between the listing and the confirmation, what is
    /// lost is not an empty file: it is the entire editing session the
    /// human writes afterward, in a directory they were not looking at.
    /// There the anchor is worth MORE, not less.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_anchor: Option<crate::DirAnchor>,
}

/// Params of [`FS_MKDIR`] (#104).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsMkdirParams {
    /// Directory to create, COMPLETE (the last segment is the new name).
    /// The parent must exist; there is no `-p`.
    pub path: VPath,
}

/// Result of [`FS_COPY`], [`FS_MOVE`], [`FS_DELETE`] and [`FS_MKDIR`]: the
/// created Task. Progress arrives via [`TASK_PROGRESS`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsTaskResult {
    /// Id of the queued Task.
    pub task_id: TaskId,
}

/// Params of [`FS_SEARCH`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsSearchParams {
    /// Root of the walk (entire subtree).
    pub root: VPath,
    /// Glob over the NAME (last segment), e.g. `*.rs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_glob: Option<String>,
    /// Regex over the name. Mutually exclusive with `name_glob`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_regex: Option<String>,
    /// Literal text to search for in CONTENT (multi-encoding: the needle
    /// gets transcoded, the haystack is never decoded whole).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Regex over content (only files the detector reports as text).
    /// Mutually exclusive with `content`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_regex: Option<String>,
    /// Case sensitive (default false; name matching happens over the lossy
    /// NFC form — same discipline as the quick search).
    #[serde(default)]
    pub case_sensitive: bool,
    /// Hit limit: once reached, the Task completes (`Completed`, not
    /// `Failed`). There is no `truncated` flag on the wire — the client
    /// infers truncation by comparing the total hits received against
    /// `max_hits` (== implies truncated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_hits: Option<u32>,

    // ---- The 0.81.0 filters ---------------------------------------
    //
    // All optional and all in the safe direction: absent, the search is
    // exactly 0.80's. What is NOT safe is sending them to a daemon that
    // does not know them, because it would ignore them and answer with
    // MORE than what was asked — a filter that is not applied returns the
    // superset, and whoever searched for "files under a meg" gets the
    // whole tree believing that was the result. The SDK refuses with
    // `Unsupported` instead of sending them, just like `journal.undo_after`'s
    // ceiling (0.80.0).
    /// The entry CLASSES that count as a result. Empty = all.
    ///
    /// It is the "I'm looking for a folder, not a file with that name"
    /// question, which in a code tree is the difference between three
    /// results and three hundred. It does NOT affect the WALK: a folder
    /// that does not count as a result is still descended into, because
    /// what is searched for may be inside it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<EntryKind>,
    /// Minimum size in bytes, inclusive. An entry whose size the provider
    /// cannot say does NOT pass a size filter: filtering is asserting, and
    /// "I don't know" is not "yes".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size: Option<u64>,
    /// Maximum size in bytes, inclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size: Option<u64>,
    /// Modified at or after this instant (ms since epoch). Same rule as
    /// size: no date does not pass the filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_after: Option<i64>,
    /// Modified at or before this instant (ms since epoch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_before: Option<i64>,
    /// Subtrees that are NOT walked, by their path. At most
    /// [`SEARCH_EXCLUDES_MAX`].
    ///
    /// Different from the `excluded` the POLICY already applied to an
    /// agent: that is what cannot be read, and this is what the human does
    /// not want to look at. The two add up, and the policy's cannot be
    /// lifted from here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_roots: Vec<VPath>,
    /// Folder names that are not descended into, AT ANY LEVEL: `target`,
    /// `node_modules`, `.git`.
    ///
    /// It is the filter that turns a useless search into a useful one, and
    /// it is named by NAME and not by path precisely because the unwanted
    /// folder appears a hundred times in places that are not known ahead of
    /// time. These are globs over the last segment, with the same
    /// discipline as `name_glob`, and at most [`SEARCH_EXCLUDES_MAX`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_names: Vec<String>,
    /// The CONTENT match has to be a whole word.
    ///
    /// Without this, searching for `set` in a code tree returns `offset`,
    /// `settings` and `subset`, which is half the file.
    ///
    /// It does not travel when it is `false`, its default: this way an
    /// ordinary search's JSON stays byte-for-byte 0.80's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub whole_word: bool,
    /// Walk subdirectories. Absent = `true`, which is what it used to do.
    ///
    /// It exists because "what is HERE" and "what is under here" are two
    /// questions, and the second one over `/` is an afternoon's work.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub recursive: bool,
    /// The encoding to read CONTENT with, by its WHATWG name
    /// (`"utf-8"`, `"windows-1252"`, `"shift_jis"`).
    ///
    /// Absent = automatic, which is what there is: the NEEDLE is
    /// transcoded to several candidates and the haystack is not decoded
    /// whole. That gets it right almost always and that is why it is the
    /// default; this is for when it does not, the same reason the viewer
    /// lets you force its own. An unrecognized name is a request error, not
    /// a search that stays silent and finds nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
}

/// How many exclusions a search admits, counting
/// [`FsSearchParams::exclude_roots`] and [`FsSearchParams::exclude_names`]
/// separately.
///
/// The bound belongs to the PROTOCOL and not to the engine, and that is why
/// it is here: a limit not in the field's rustdoc is not part of the
/// contract, and whoever writes a client from `proto.schema.json` cannot
/// guess it.
///
/// It exists because `fs.search` is reachable by an AGENT within its read
/// scope, and every excluded name compiles a glob — and with it a regex
/// with its anti-ReDoS budget — in the task serving the connection, BEFORE
/// any Task exists to count against the live-tasks ceiling. Without a
/// bound, a request that fits in one frame buys a million compilations and
/// then a linear pass over every entry of the walk.
///
/// 256 because the real list — `target`, `node_modules`, `.git`, `vendor`,
/// `dist` — has a handful of entries, and two orders of magnitude above
/// what nobody writes by hand is plenty of room for what a tool generates.
pub const SEARCH_EXCLUDES_MAX: usize = 256;

impl FsSearchParams {
    /// A search under `root` with NO criterion or filter at all: everything
    /// else at its absent value, including `recursive: true`.
    ///
    /// It is not a `Default` — `root` has none, and a search without a root
    /// means nothing — but the base a caller builds their own on:
    ///
    /// ```
    /// use norte_proto::methods::FsSearchParams;
    /// use norte_proto::VPath;
    ///
    /// let root = VPath::parse("file:///casa").expect("vpath");
    /// let p = FsSearchParams {
    ///     name_glob: Some("*.rs".to_owned()),
    ///     ..FsSearchParams::new(root)
    /// };
    /// assert!(p.recursive, "walking subdirectories is the default");
    /// assert!(p.content.is_none());
    /// ```
    ///
    /// It exists so a new protocol field does not break every frontend: the
    /// ten filters of 0.81.0 touched two places that spelled out the whole
    /// struct, and the next one would touch the same ones.
    #[must_use]
    pub fn new(root: VPath) -> Self {
        Self {
            root,
            name_glob: None,
            name_regex: None,
            content: None,
            content_regex: None,
            case_sensitive: false,
            max_hits: None,
            kinds: Vec::new(),
            min_size: None,
            max_size: None,
            mtime_after: None,
            mtime_before: None,
            exclude_roots: Vec::new(),
            exclude_names: Vec::new(),
            whole_word: false,
            recursive: true,
            encoding: None,
        }
    }
}

/// `true`, for the `serde(default)` of a boolean whose absent value is yes.
const fn default_true() -> bool {
    true
}

/// If it is `true`, for the `skip_serializing_if` of those same ones: a
/// value that matches the default does not travel, so an ordinary search's
/// JSON stays byte-for-byte 0.80's.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "the signature serde requires for skip_serializing_if"
)]
const fn is_true(b: &bool) -> bool {
    *b
}

/// A batch of [`SEARCH_HITS`] results. `matches` aligned 1:1 with `entries`
/// when the search is content-based (None if name-only).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHits {
    /// Owning Task (correlates with `fs.search` → `task_id`).
    pub task_id: TaskId,
    /// Matching entries.
    pub entries: Vec<Entry>,
    /// Content match context, aligned with `entries`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matches: Option<Vec<MatchInfo>>,
}

/// Context of ONE content match.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchInfo {
    /// Line (1-based) of the first match, if it was computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    /// The match's line, lossy-decoded and sanitized AT THE SOURCE
    /// (control/bidi/invisible chars masked to `U+FFFD`, then trimmed to a
    /// fixed char limit). The consumer can paint it directly — raw ANSI or
    /// bidi overrides never arrive over the wire — though the TUI still
    /// passes it through `detail_for_bar` as a belt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// Params of [`INDEX_BUILD`] (M4): root to index.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexBuildParams {
    /// Root of the subtree to (re)index.
    pub root: VPath,
}

/// Result of [`INDEX_BUILD`] when the Task completes (M4).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexBuildResult {
    /// Entries indexed (inserted or updated).
    pub indexed: u64,
    /// Rows swept (paths that no longer existed).
    pub removed: u64,
}

/// Params of [`INDEX_QUERY`] (M4).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexQueryParams {
    /// Root whose index is queried.
    pub root: VPath,
    /// Free-form user text (sanitized into an FTS5 query in the core).
    pub text: String,
    /// Result limit.
    pub limit: u32,
}

/// An [`INDEX_QUERY`] hit (M4). `path` in raw bytes via [`VPath`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexHit {
    /// Full path of the result.
    pub path: VPath,
    /// Entry type.
    pub kind: EntryKind,
    /// Size (`None` for dirs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// mtime in ms since epoch (`None` if unknown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_ms: Option<i64>,
}

/// Result of [`INDEX_QUERY`] (M4): hits ordered by relevance.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexQueryResult {
    /// Hits (bm25, most relevant first).
    pub hits: Vec<IndexHit>,
}

/// Params of [`INDEX_EMBED`] (0.33.0, M4-IA-2).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEmbedParams {
    /// Root ALREADY indexed with `index.build` (same exact key).
    pub root: VPath,
}

/// Params of [`INDEX_SEARCH_SEMANTIC`] (0.33.0, M4-IA-2).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexSearchSemanticParams {
    /// Root to query; absent ⇒ all roots of the index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<VPath>,
    /// Natural-language query (goes out to the AI provider).
    pub query: String,
    /// Maximum hits; the server trims to [`INDEX_SEMANTIC_MAX_K`].
    pub k: u32,
}

/// A semantic hit: path + cosine similarity. No `Eq` (unlike its `index.*`
/// siblings): `score` is `f64`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticHit {
    /// File path (wire encoding).
    pub path: VPath,
    /// Cosine similarity in `[-1, 1]` (higher = closer affinity).
    /// Always finite: the server never emits NaN/Infinity (a belt in the
    /// engine — a non-finite value would serialize as null and poison the
    /// response).
    pub score: f64,
}

/// Result of [`INDEX_SEARCH_SEMANTIC`] (0.33.0), best first.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexSearchSemanticResult {
    /// Hits ordered by descending score.
    pub hits: Vec<SemanticHit>,
}

/// Params of [`AI_RENAME_PLAN`] (M4-IA, ADR 0031).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRenamePlanParams {
    /// Directory whose basenames are sent to the provider (after the AI gate).
    pub dir: VPath,
    /// User instruction.
    pub instruction: String,
    /// The basenames the plan is requested for, inside [`Self::dir`]
    /// (0.62.0, #121).
    ///
    /// EMPTY — and absent — is the whole directory, which is what 0.61 used
    /// to do. With first-class selection (#103), marking five files and
    /// requesting a plan sent the directory's thousand files to the
    /// provider: more than what the human pointed at, and the AI gate
    /// exists precisely to bound what leaves the machine.
    ///
    /// These are BASE NAMES and not paths: the plan is for one directory,
    /// and a path here would open the door to requesting a plan over what
    /// is in another one. A name not in the listing is ignored — the
    /// listing rules, and rejecting the whole plan for an entry deleted
    /// between marking and requesting would punish the reader for a race.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

/// How many names [`AiRenamePlanParams::names`] admits (0.62.0, #121).
///
/// A list that arrives from outside, and that the engine walks for every
/// entry of the listing, needs a bound, or a 16 MiB frame of one-byte
/// names turns a plan into a quadratic loop. The number is the same as a
/// batch of paths ([`FS_SET_MODE_MAX_PATHS`]) because it measures the same
/// thing: how many things a human selects at once.
pub const AI_RENAME_NAMES_MAX: usize = 4096;

/// A pair of the [`AI_RENAME_PLAN`] plan. BASE names, UTF-8 guaranteed: the
/// engine rejects hostile names fail-loud BEFORE calling the provider and
/// validates `to` as a `Segment` (no `/`, `..`, NUL, `!` or `\`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRenameEntry {
    /// Existing name in `dir`.
    pub from: String,
    /// Proposed destination name.
    pub to: String,
}

/// Result of [`AI_RENAME_PLAN`]: the REVIEWABLE plan (spec §9). Empty = the
/// model did not propose any changes. The plan is the product: applying it
/// is N governed [`FS_MOVE`]s; this method never mutates.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRenamePlanResult {
    /// from→to pairs (only the ones that change name).
    pub entries: Vec<AiRenameEntry>,
    /// Why the producer did NOT propose anything (0.68.0, #332): the
    /// sentence from a `renamer` plugin that refused ("approve my
    /// `location` capability"). THIRD-PARTY text, already masked and
    /// bounded by the daemon; the frontend shows it as a message, never
    /// interprets it. RECEIVER rule: if there is a reason, `entries` does
    /// not count — no daemon produces a plan with both a reason and pairs,
    /// and the frontend keeps the reason. Omitted when there is no reason,
    /// so 0.67's wire did not move; a 0.67 client ignores it and sees an
    /// empty plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

/// Params of [`AI_ORGANIZE_PLAN`] (0.77.0, phase 8 of the WOW program).
///
/// The same three fields as [`AiRenamePlanParams`] and for the same reason:
/// organizing is renaming with permission to move into a subdirectory, so
/// what is asked of the provider — a directory, an instruction and the
/// names to act on — does not change.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiOrganizePlanParams {
    /// Directory whose basenames are sent to the provider (after the AI gate).
    pub dir: VPath,
    /// User instruction.
    pub instruction: String,
    /// The basenames the plan is requested for, inside [`Self::dir`]. Empty
    /// is the whole directory. Same limit as the rename plan
    /// ([`AI_RENAME_NAMES_MAX`]), because it measures the same thing: how
    /// many things a human points at once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

/// A move in the organize plan: what there is and where it goes.
///
/// The difference from [`AiRenameEntry`] — and the reason this is a
/// different family and not just one more field — is
/// [`Self::proposed_rel`]: a destination that can carry subdirectories.
/// That turns the plan into something that also CREATES directories, and
/// therefore into a different operation, with a different reversal and a
/// different review dialog.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizeMove {
    /// BASE name existing in the plan's directory.
    pub current: String,
    /// Where it goes, RELATIVE to that same directory, with `/` as
    /// separator.
    ///
    /// Can carry subdirectories (`facturas/2026/marzo.pdf`), and that is
    /// this phase's whole value — and its whole risk. What it CANNOT be:
    /// absolute, empty, with a `.` or `..` segment, with NUL, or ending in
    /// `/`. Every segment has to be a valid [`crate::Segment`], and the
    /// CORE checks that before creating anything
    /// ([`validar_proposed_rel`]): a plan is produced by a third party — a
    /// model or a plugin — and a `..` here is a write outside the
    /// directory the human was looking at.
    pub proposed_rel: String,
}

/// How many segments an [`OrganizeMove::proposed_rel`] can have.
///
/// A bound, and not "as many as it wants": every intermediate segment is a
/// directory that has to be created, and a thousand-level proposal is a
/// thousand `fs.create`s nobody asked for. Eight is deeper than any
/// organization a human would review at a glance, which is the operation
/// this serves.
pub const ORGANIZE_MAX_DEPTH: usize = 8;

/// Why a `proposed_rel` is not valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum OrganizeRelError {
    /// Empty, or only separators.
    #[error("the relative destination is empty")]
    Empty,
    /// Starts with `/`: it is an absolute path in disguise.
    #[error("the relative destination cannot be absolute")]
    Absolute,
    /// More than [`ORGANIZE_MAX_DEPTH`] segments.
    #[error("the relative destination has more than {ORGANIZE_MAX_DEPTH} levels")]
    TooDeep,
    /// A segment that is not valid (`.`, `..`, empty, with NUL…).
    #[error("the relative destination has an invalid segment")]
    BadSegment,
}

/// Validates an [`OrganizeMove::proposed_rel`] and returns its segments.
///
/// Lives in the protocol, and not in the core, because both ends need it
/// with the SAME answer: the core before creating a single directory, and a
/// frontend so as not to paint as reviewable a plan the core is going to
/// reject. Two validations for the same rule diverge, and the one that
/// relaxes is always the one that does not delete files.
///
/// What it guarantees: no segment is `.` or `..`, none carries NUL or `/`,
/// there are no empties (meaning: it neither starts nor ends with `/`, nor
/// carries `//`), and it does not exceed [`ORGANIZE_MAX_DEPTH`].
///
/// # Errors
/// [`OrganizeRelError`].
///
/// ```
/// use norte_proto::methods::validar_proposed_rel;
/// assert!(validar_proposed_rel("facturas/2026/marzo.pdf").is_ok());
/// assert!(validar_proposed_rel("../fuera.txt").is_err());
/// assert!(validar_proposed_rel("/etc/passwd").is_err());
/// assert!(validar_proposed_rel("a//b").is_err());
/// ```
pub fn validar_proposed_rel(rel: &str) -> Result<Vec<crate::Segment>, OrganizeRelError> {
    if rel.is_empty() {
        return Err(OrganizeRelError::Empty);
    }
    if rel.starts_with('/') {
        return Err(OrganizeRelError::Absolute);
    }
    let pieces: Vec<&str> = rel.split('/').collect();
    if pieces.len() > ORGANIZE_MAX_DEPTH {
        return Err(OrganizeRelError::TooDeep);
    }
    pieces
        .into_iter()
        .map(|s| crate::Segment::new(s.as_bytes()).map_err(|_| OrganizeRelError::BadSegment))
        .collect()
}

/// Result of [`AI_ORGANIZE_PLAN`]: the REVIEWABLE plan. Empty = the producer
/// proposed nothing.
///
/// The plan is the product; applying it is [`FS_ORGANIZE`], which is the
/// one that mutates. This method never touches a byte.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiOrganizePlanResult {
    /// The proposed moves. Only the ones that change something.
    pub moves: Vec<OrganizeMove>,
    /// Why the producer did NOT propose anything, with the same contract as
    /// [`AiRenamePlanResult::refused`]: THIRD-PARTY text, already masked
    /// and bounded by the daemon, and if there is a reason `moves` does not
    /// count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    /// The token that must be returned in [`FsOrganizeParams::plan_hash`]
    /// to apply EXACTLY these moves. `None` when there is no plan to apply
    /// (refused, or with no moves).
    ///
    /// **Travels with the plan, and not in a second method**, which is the
    /// difference from batch renaming: there the hash is given by
    /// `fs.rename_batch_plan` because that method also checks collisions
    /// and decides whether the batch is applicable. Here there is nothing
    /// to check besides the plan's shape — and the core already did that to
    /// be able to propose it — so a second round trip would only add a
    /// window in which the human looks at a plan that cannot yet be
    /// approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_hash: Option<PlanHash>,
}

/// Params of [`FS_ORGANIZE`] (0.77.0, phase 8): apply an organize plan.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsOrganizeParams {
    /// The directory all the moves live in.
    pub dir: VPath,
    /// What is going to be moved, the SAME intent that produced `plan_hash`.
    pub moves: Vec<OrganizeMove>,
    /// The hash of the plan the human approved. Whether it matches is what
    /// the core checks; not matching is
    /// [`Error::PlanStale`](crate::Error::PlanStale).
    pub plan_hash: PlanHash,
}

/// Why a string is not a [`PlanHash`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PlanHashError {
    /// Length different from [`PLAN_HASH_LEN`].
    #[error("plan hash must be exactly {PLAN_HASH_LEN} characters")]
    BadLength,
    /// Some character is not LOWERCASE hex (`0`-`9`, `a`-`f`).
    #[error("plan hash must be lowercase hex (0-9, a-f)")]
    NotLowercaseHex,
}

/// The hash of a rename plan: sha256 in LOWERCASE hex, exactly
/// [`PLAN_HASH_LEN`] characters.
///
/// It is a TYPE and not a `String` for the same reason [`Segment`] is one:
/// the rule "a wrong shape is a PARAMS error, not
/// [`Error::PlanStale`](crate::Error::PlanStale)" written only in prose
/// gets re-implemented in the daemon's dispatch, again in the MCP bridge
/// and again wherever a frontend returns the hash it received — and what
/// one of those copies drops is exactly the lowercasing, the detail that
/// makes two spellings of the SAME hash compare differently. Validated on
/// DESERIALIZATION, it holds once for every layer: an invalid value never
/// gets to exist.
///
/// ```
/// use norte_proto::methods::PlanHash;
/// let h = PlanHash::parse(&"ab".repeat(32)).expect("64 lowercase hex");
/// assert_eq!(h.as_str().len(), 64);
/// assert_eq!(serde_json::to_string(&h).expect("json"), format!("\"{h}\""));
/// // Uppercase, length and garbage: rejected at construction...
/// assert!(PlanHash::parse(&"AB".repeat(32)).is_err());
/// assert!(PlanHash::parse("00").is_err());
/// // ...and over the wire, which is where it matters.
/// assert!(serde_json::from_str::<PlanHash>(r#""00""#).is_err());
/// assert!(serde_json::from_str::<PlanHash>(&format!(r#""{}""#, "ab".repeat(32))).is_ok());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PlanHash(String);

impl PlanHash {
    /// Validates and builds from the hex form. It is the ONLY path: the
    /// core uses it over the hex its hasher produces, and the wire uses it
    /// when deserializing.
    ///
    /// # Errors
    /// [`PlanHashError::BadLength`] if it is not [`PLAN_HASH_LEN`] long;
    /// [`PlanHashError::NotLowercaseHex`] if some character is not `0`-`9`
    /// or `a`-`f`.
    pub fn parse(hex: &str) -> Result<Self, PlanHashError> {
        if hex.len() != PLAN_HASH_LEN {
            return Err(PlanHashError::BadLength);
        }
        if !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(PlanHashError::NotLowercaseHex);
        }
        Ok(Self(hex.to_owned()))
    }

    /// From a sha256's RAW digest: the lowercase hex form, with no way to
    /// get it wrong.
    ///
    /// It is the path of whoever PRODUCES a hash, and it exists so there is
    /// no hex encoder per crate: every copy is a chance to write uppercase
    /// — the detail that makes two spellings of the same hash compare
    /// differently — and it also forces an `expect` over
    /// [`PlanHash::parse`] that is not needed here, because 32 bytes cannot
    /// give anything other than 64 characters of `0`-`9` and `a`-`f`.
    /// [`PlanHash::parse`] remains the path of whoever RECEIVES it.
    ///
    /// ```
    /// use norte_proto::methods::PlanHash;
    /// let h = PlanHash::from_digest(&[0xab; 32]);
    /// assert_eq!(h.as_str(), "ab".repeat(32));
    /// assert_eq!(h, PlanHash::parse(&"ab".repeat(32)).expect("hex"));
    /// ```
    #[must_use]
    pub fn from_digest(digest: &[u8; 32]) -> Self {
        Self(crate::hashing::hex_lower(digest))
    }

    /// The hex form, exactly as it travels.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PlanHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PlanHash {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let hex = String::deserialize(deserializer)?;
        Self::parse(&hex).map_err(serde::de::Error::custom)
    }
}

// The serde is by hand (validates on deserialize), so the schema is too: it
// is a string with a pattern, and the pattern comes from `PLAN_HASH_LEN` so
// it cannot drift apart from the validator.
#[cfg(feature = "schema")]
impl schemars::JsonSchema for PlanHash {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PlanHash".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "minLength": PLAN_HASH_LEN,
            "maxLength": PLAN_HASH_LEN,
            "pattern": format!("^[0-9a-f]{{{PLAN_HASH_LEN}}}$"),
            "description": "sha256 of a rename plan's conclusions, as exactly 64 \
                            LOWERCASE hex digits. Uppercase is rejected: two \
                            spellings of one hash must not compare differently. A \
                            string of any other shape is a params error, never \
                            `plan_stale`.",
        })
    }
}

/// A REQUESTED rename inside a directory: base names, not paths.
///
/// Unlike [`AiRenameEntry`] (where the AI provider guarantees UTF-8 and the
/// engine rejects the rest fail-loud), names here are [`Segment`]: a batch
/// of renames is exactly where a non-UTF8 name has to survive byte for byte
/// (hard rule 1).
///
/// ```
/// use norte_proto::{Segment, methods::RenamePair};
/// // A name that is NOT UTF-8 on the left: the wire escapes it and the
/// // bytes come back intact, exactly what a `String` could not promise.
/// let p = RenamePair {
///     from: Segment::new(b"caf\xff.txt".to_vec()).expect("segment"),
///     to: Segment::new(b"cafe.txt".to_vec()).expect("segment"),
/// };
/// let json = serde_json::to_string(&p).expect("json");
/// assert_eq!(json, r#"{"from":"caf%FF.txt","to":"cafe.txt"}"#);
/// let back: RenamePair = serde_json::from_str(&json).expect("json");
/// assert_eq!(back.from.as_bytes(), b"caf\xff.txt");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenamePair {
    /// Existing name in the directory.
    pub from: Segment,
    /// Proposed name.
    pub to: Segment,
}

/// A step of the ORDERED plan. `temp` marks a step that is planner
/// MACHINERY (breaking a cycle), not something the user asked for.
///
/// A permutation `a→b, b→a` is THREE steps, and the temporary appears in
/// two: `a → .norte-rename-XXXXXXXX-0` (`temp`), `b → a`, and
/// `.norte-rename-XXXXXXXX-0 → b` (`temp`). Without the third, the file
/// that started as `a` is left parked under the machine name.
///
/// A step does NOT say which pair it comes from, and that is deliberate:
/// steps are opaque MACHINERY. A frontend paints the user's pairs — which
/// it already has — plus the verdicts, which do carry `pair_index`; ORDER
/// is the core's business (see [`FS_RENAME_BATCH_PLAN`]), and a temporary
/// splits one pair into two steps precisely because it is one. If a
/// frontend ever proves it needs the mapping, adding the field is an
/// ADDITIVE change — the correct shape for a need that has not been shown
/// yet.
///
/// ```
/// use norte_proto::{Segment, methods::RenameStep};
/// let s = RenameStep {
///     from: Segment::new(b"caf\xff.txt".to_vec()).expect("segment"),
///     to: Segment::new(b"cafe.txt".to_vec()).expect("segment"),
///     temp: false,
/// };
/// assert_eq!(serde_json::to_string(&s).expect("json"),
///            r#"{"from":"caf%FF.txt","to":"cafe.txt","temp":false}"#);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameStep {
    /// Name before this step.
    pub from: Segment,
    /// Name after this step.
    pub to: Segment,
    /// `true` if EITHER side of this step is a temporary name owned by the
    /// planner — a property of the STEP, not of `to`: the step that takes
    /// the file out of the temporary carries it in `from` and is machinery
    /// just the same. A frontend never presents it as a user proposal.
    pub temp: bool,
}

/// Why a plan cannot be executed. CLOSED vocabulary: the core never
/// invents a class.
///
/// N/N-1 tolerance (ADR 0004/0005, same pattern as
/// [`ConflictKind`](crate::ConflictKind)): an unknown class deserializes to
/// [`RenameCollisionKind::Unknown`] instead of blowing up parsing the
/// whole plan. It matters even though the vocabulary is born closed: a
/// 0.36 client negotiates with a 0.37 daemon, and a new verdict there
/// cannot leave it with no plan to paint — it degrades to "rejected,
/// reason I don't understand".
///
/// ```
/// use norte_proto::methods::RenameCollisionKind;
/// assert_eq!(
///     serde_json::to_string(&RenameCollisionKind::AbsentSource).expect("json"),
///     r#""absent_source""#
/// );
/// let future: RenameCollisionKind =
///     serde_json::from_str(r#""future_class""#).expect("json");
/// assert_eq!(future, RenameCollisionKind::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RenameCollisionKind {
    /// A PRIOR pair of the same batch makes this one impossible: it took
    /// the destination, or it took the SOURCE. Both are measured with the
    /// directory's collision equivalence (NFC, and case folding where case
    /// folds), not byte for byte, so two pairs naming distinct files that
    /// directory does not distinguish also fall here.
    Internal,
    /// The destination already exists and NO pair of the batch is going to
    /// move it out of the way. That a pair exists whose source is that name
    /// is not enough: if that pair is a no-op, or its source is a TWIN file
    /// of the one in the way (`café` in NFC versus `café` in NFD), the file
    /// is still there.
    External,
    /// The pair's source is not in the directory (the plan was built
    /// against a stale listing).
    AbsentSource,
    /// The source does not match EXACTLY any name in the directory and it
    /// folds onto TWO OR MORE at once — two files that directory's rules do
    /// not distinguish, typically `café` in NFC and in NFD coexisting on an
    /// ext4, or `Foo` and `foo` in a listing that contradicts its own
    /// capabilities.
    ///
    /// The source is NOT missing: there is too much of it. The core does
    /// not guess which of the two was intended, because getting it right
    /// half the time is worse than doing nothing. What resolves THIS
    /// verdict is writing the name BYTE FOR BYTE as `fs.list` returns it:
    /// with the exact spelling there is an exact match and the source is
    /// identified.
    ///
    /// Note, this is not a master key for the twin directory. Renaming
    /// BOTH twins in the same batch is still impossible, because collisions
    /// are measured folded and the second one gets an [`Self::Internal`].
    /// They have to be sent in separate batches.
    AmbiguousSource,
    /// Class from a newer protocol (deserialization fallback). The core
    /// NEVER emits it.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

/// A rejected name plus its verdict, ADDRESSABLE to the pair that caused it.
///
/// ```
/// use norte_proto::Segment;
/// use norte_proto::methods::{RenameCollision, RenameCollisionKind};
/// let c = RenameCollision {
///     pair_index: 1,
///     name: Segment::new(b"ep01.mkv".to_vec()).expect("segment"),
///     kind: RenameCollisionKind::Internal,
/// };
/// assert_eq!(serde_json::to_string(&c).expect("json"),
///            r#"{"pair_index":1,"name":"ep01.mkv","kind":"internal"}"#);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameCollision {
    /// Index, within the request's `pairs`, of the pair this verdict
    /// REJECTS. For `Internal` it is the LATER pair in `pairs`'s order (the
    /// first one gets flagged on its own if it is also rejected). There is
    /// no "winning" pair a frontend can paint as accepted: a plan with
    /// verdicts executes NOTHING.
    ///
    /// It is defined for every class, including future ones, and that is
    /// its reason: `name` changes meaning with `kind` (see below), so under
    /// [`RenameCollisionKind::Unknown`] a client would not know what it is
    /// looking at. With the index it can always point at the guilty row,
    /// even without understanding the verdict — which is exactly what the
    /// fallback promises.
    pub pair_index: u32,
    /// The offending name. WHICH name depends on `kind`:
    ///
    /// - `Internal` — the DESTINATION this pair is not going to get;
    /// - `External` — the file IN THE WAY, written as the directory writes
    ///   it, which need not be as the request wrote it: an NFD twin
    ///   shadows an NFC destination, and what the human needs to see is
    ///   the twin, not an echo of what they already typed;
    /// - `AbsentSource` / `AmbiguousSource` — the SOURCE, exactly as
    ///   whoever requested it sent it, because that is the text they have
    ///   to correct.
    ///
    /// The one that depends on nothing is `pair_index`.
    pub name: Segment,
    /// The verdict.
    pub kind: RenameCollisionKind,
}

/// Params of [`FS_RENAME_BATCH_PLAN`] (0.36.0).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsRenameBatchPlanParams {
    /// The directory all the pairs live in.
    pub dir: VPath,
    /// The requested renames. More than [`FS_RENAME_BATCH_MAX_PAIRS`] is a
    /// params error (`-32602`), not a trim.
    #[cfg_attr(
        feature = "schema",
        schemars(extend("maxItems" = FS_RENAME_BATCH_MAX_PAIRS))
    )]
    pub pairs: Vec<RenamePair>,
}

/// Result of [`FS_RENAME_BATCH_PLAN`] (0.36.0).
///
/// **A plan that cannot be executed carries no steps** — the core does not
/// half-order a plan it is not going to execute (design decision 4). The
/// NORMATIVE invariant, with its exact direction, is stated ONCE, on
/// `executable`.
///
/// ```
/// use norte_proto::methods::{FsRenameBatchPlanResult, PlanHash};
/// let r = FsRenameBatchPlanResult {
///     steps: vec![],
///     collisions: vec![],
///     executable: true,
///     plan_hash: PlanHash::parse(&"0".repeat(64)).expect("hex"),
/// };
/// let json = serde_json::to_value(&r).expect("json");
/// // Empty `collisions` is an empty list, never an absent key.
/// assert_eq!(json["collisions"], serde_json::json!([]));
/// // The hash travels as the bare string, without the newtype's wrapper.
/// assert_eq!(json["plan_hash"], serde_json::json!("0".repeat(64)));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsRenameBatchPlanResult {
    /// Steps in execution ORDER, temporaries included. Bounded by
    /// [`FS_RENAME_BATCH_MAX_PAIRS`]: at most one non-null pair plus one
    /// temporary per cycle, and a cycle consumes at least two pairs — the
    /// hard ceiling is `pairs * 3 / 2`. No constant of its own: the pair
    /// limit bounds it.
    ///
    /// EMPTY whenever `executable` is `false` (see the invariant on that
    /// field).
    pub steps: Vec<RenameStep>,
    /// Everything that stops the plan; empty when `executable`. At most ONE
    /// entry per pair, so its ceiling is exactly
    /// [`FS_RENAME_BATCH_MAX_PAIRS`] — not `steps`'s, which is bigger
    /// because temporaries add steps without adding pairs.
    ///
    /// It is the EXPLANATION, not the verdict: what decides whether it can
    /// execute is `executable`.
    pub collisions: Vec<RenameCollision>,
    /// `true` when the plan can be executed as-is. NORMATIVE field: the
    /// frontend disables confirming with `!executable`, and deduces
    /// nothing from `collisions`.
    ///
    /// It is derivable from `collisions.is_empty()` TODAY, and this field
    /// still rules: a future verdict could stop a plan with no offending
    /// name to list, and a client deducing from the list would execute it.
    ///
    /// INVARIANT (the core maintains it, a client may assume it):
    /// `executable == false` ⟹ `steps` empty, and `collisions` non-empty ⟹
    /// `executable == false`. Note the DIRECTION: what empties `steps` is
    /// `!executable`, not the presence of verdicts — so the invariant
    /// still holds for that future verdict with no name to list, and a
    /// client that ignored this field and blindly executed `steps` would,
    /// in every case, have nothing to execute.
    pub executable: bool,
    /// sha256 in lowercase hex over the plan's CONCLUSIONS (directory,
    /// ordered pairs, resulting steps, verdicts, case and normalization
    /// flags). Return it in [`FsRenameBatchParams`]: the core re-plans and
    /// compares. It is a hex string on purpose — readable in a log, no
    /// base64 ambiguity, no bytes on the wire.
    ///
    /// Its type ALREADY imposes the shape ([`PLAN_HASH_LEN`] lowercase hex
    /// characters): a string of another shape is a PARAMS error that dies
    /// at deserialization, not
    /// [`Error::PlanStale`](crate::Error::PlanStale) — "doesn't match" and
    /// "the directory changed" are different things, and answering the
    /// second one to whoever sent garbage lies to them about the state of
    /// the world.
    pub plan_hash: PlanHash,
}

/// Params of [`FS_RENAME_BATCH`] (0.36.0).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsRenameBatchParams {
    /// The directory all the pairs live in.
    pub dir: VPath,
    /// The requested renames — the SAME intent that produced `plan_hash`.
    /// Limit [`FS_RENAME_BATCH_MAX_PAIRS`], same as in the plan.
    #[cfg_attr(
        feature = "schema",
        schemars(extend("maxItems" = FS_RENAME_BATCH_MAX_PAIRS))
    )]
    pub pairs: Vec<RenamePair>,
    /// The hash of the plan the human approved. That the SHAPE is valid is
    /// guaranteed by [`PlanHash`] at deserialization; that the CONTENT
    /// matches is what the core checks, and not matching is
    /// [`Error::PlanStale`](crate::Error::PlanStale).
    pub plan_hash: PlanHash,
}

/// A step of a batch that was left APPLIED and that the core could not put
/// back in its place (0.36.0). In other words: the directory is
/// half-renamed and this says where to look.
///
/// Both sides travel as an absolute [`VPath`] and not as [`Segment`]:
/// whoever reads this is looking for a file, and giving it the bare name
/// would force it to reconstruct the request's directory to be able to go
/// get it.
///
/// ```
/// use norte_proto::{VPath, methods::RenameStuckStep};
/// let s = RenameStuckStep {
///     from: VPath::parse("file:///fotos/a").expect("path"),
///     to: VPath::parse("file:///fotos/b").expect("path"),
///     pair_index: 0,
///     error: norte_proto::Error::Io { retryable: false },
///     journalled: true,
///     still_applied: 1,
/// };
/// let json = serde_json::to_value(&s).expect("json");
/// assert_eq!(json["to"], serde_json::json!("file:///fotos/b"));
/// assert_eq!(json["journalled"], serde_json::json!(true));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameStuckStep {
    /// The name the file had BEFORE the batch: where it could not be
    /// returned to.
    pub from: VPath,
    /// The name the file carries NOW. It is the one to search for.
    pub to: VPath,
    /// Index, within the request's `pairs`, of the pair this step descends
    /// from. A temporary splits one pair into two steps, so two distinct
    /// stuck steps can cite the same pair.
    pub pair_index: u32,
    /// Why the reversal was refused, or why the step's destination is
    /// unknown (protocol error taxonomy).
    pub error: crate::Error,
    /// Is there a journal entry behind this step?
    ///
    /// Decides WHO has to clean up. `true`: the entry describes the rename,
    /// so a later `policy.undo_session` can finish it off once the obstacle
    /// disappears. `false`: the rename took effect but its entry never
    /// landed (hard rule 4 — the journal does not know it happened), so no
    /// undo will find it and only a human can undo it.
    pub journalled: bool,
    /// How many steps of that batch are still applied, this one included.
    pub still_applied: u64,
}

/// Which digest function is requested (0.59.0, #311).
///
/// Today only one, and the enum exists anyway: a field with a single value
/// declares that the response depends on it, and adding `sha512` or `md5`
/// later is then additive. Without the field, the first new function would
/// force guessing what a digest already on screen was computed with.
///
/// **Strict on purpose**: it carries no `serde(other)`. An algorithm this
/// daemon does not know is a params error, never a digest computed with
/// something else — which is exactly the answer that would make checking
/// anything pointless.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChecksumAlgo {
    /// SHA-256, in lowercase hex. The one from `sha256sum`, the format one
    /// finds out there.
    #[default]
    Sha256,
}

/// Params of [`FS_SET_MODE`] (0.60.0, #314).
///
/// ```
/// use norte_proto::methods::FsSetModeParams;
/// let p: FsSetModeParams =
///     serde_json::from_str(r#"{"paths":["file:///a.sh"],"mode":493}"#).expect("params");
/// assert_eq!(p.mode, 0o755);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsSetModeParams {
    /// The paths whose mode gets changed, exactly as requested.
    ///
    /// Without [`Self::recursive`], these are EXACTLY these: a directory
    /// changes its own mode and not that of what is inside it.
    pub paths: Vec<VPath>,
    /// The twelve permission bits, as `chmod(2)` takes them.
    ///
    /// It travels as a number and not as `rwxr-xr-x` because the wire is
    /// not an interface: text is whoever paints it's business, and two
    /// spellings of the same permission comparing differently would be a
    /// bug waiting to happen. Bits above `0o7777` are of the node's CLASS
    /// and this method does not touch them: they are rejected
    /// (`InvalidPath`), instead of being silently trimmed and changing the
    /// permission to something nobody asked for.
    pub mode: u32,
    /// Descend into [`Self::paths`]'s directories and change what is inside
    /// too (0.62.0, #315).
    ///
    /// Absent = `false`, which is what 0.60 used to do: exactly the
    /// requested paths. A client that does not send it gets the behavior it
    /// already expected.
    ///
    /// The walk has a CEILING ([`SET_MODE_RECURSIVE_MAX`]) and whatever is
    /// left unvisited is SAID in the progress, instead of trimmed silently:
    /// half a selection changed without warning is what #311 and #314
    /// reject everywhere. A symlink is not followed nor touched, inside the
    /// tree the same as outside — `chmod(2)` would follow it, and the mode
    /// saved as reversal would be the link's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recursive: bool,
    /// The mode of DIRECTORIES, when it differs from files' (0.62.0, #315).
    ///
    /// It exists because `chmod -R 644` over a tree leaves it unusable:
    /// without the execute bit, you cannot even enter a directory. It is
    /// the way out the KDE dialog Krusader uses takes — two explicit modes,
    /// instead of `chmod`'s `X`, which asks for SYMBOLIC modes and this
    /// protocol sends the mode as a number.
    ///
    /// Absent = the same [`Self::mode`] for everything, which is what
    /// `chmod -R` does and what breaks trees: the footgun is documented
    /// instead of inventing a mode nobody asked for.
    ///
    /// Without [`Self::recursive`] it means nothing and is ignored: the
    /// requested paths carry the requested mode, whatever it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir_mode: Option<u32>,
}

/// The most nodes a recursive `fs.set_mode` visits (0.62.0, #315).
///
/// [`FsSetModeParams::paths`]'s limit (4096) is over what was REQUESTED; a
/// tree is millions. Counting ahead to reject would mean walking it twice,
/// and trimming silently leaves half the selection changed — so it walks
/// up to here and what is left untouched travels in the progress
/// (not `TaskProgress::unreadable`: `entries_total` stops being the whole
/// tree and the frontend says so).
pub const SET_MODE_RECURSIVE_MAX: u64 = 100_000;

/// The bits [`FS_SET_MODE`] accepts: `chmod(2)`'s twelve permission bits.
pub const MODE_PERMISSION_BITS: u32 = 0o7777;

/// Params of [`FS_CHECKSUM`] (0.59.0, #311).
///
/// ```
/// use norte_proto::methods::{ChecksumAlgo, FsChecksumParams};
/// let p: FsChecksumParams =
///     serde_json::from_str(r#"{"paths":["file:///a.txt"]}"#).expect("params");
/// assert_eq!(p.paths.len(), 1);
/// // The algorithm can be omitted: sha256 is what everyone reads.
/// assert_eq!(p.algo, ChecksumAlgo::Sha256);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FsChecksumParams {
    /// The files to be summed. EMPTY is `-32602`: summing nothing is not a
    /// request, same criterion as [`FsDirSizeParams`].
    ///
    /// A DIRECTORY here is not walked: it comes out in the report as
    /// omitted. See [`FS_CHECKSUM`] for why hashing a tree is a different
    /// question.
    pub paths: Vec<VPath>,
    /// With which function. Absent = [`ChecksumAlgo::Sha256`].
    pub algo: ChecksumAlgo,
}

/// Why a path in the batch has no digest (0.59.0, #311).
///
/// Two reasons and not one: "could not be read" and "was not a file" are
/// fixed different ways — the first can be a permission or a file that
/// moved, the second is that you marked a folder — and a report that mixed
/// them would force the reader to go look.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChecksumMiss {
    /// The provider could not open it or could not finish reading it.
    Unreadable,
    /// Not a file (a directory, or whatever the provider says it does not read).
    NotAFile,
    /// A reason this client does not know yet.
    #[serde(other)]
    Unknown,
}

/// A path of the batch and what came out of it (0.59.0, #311).
///
/// `digest` and `miss` are mutually exclusive: either there is a sum, or
/// there is a reason. They are modeled as two optional fields and not as a
/// wire enum so an N-1 client that only looks at `digest` keeps reading the
/// report without understanding the reasons.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecksumEntry {
    /// The path, exactly as requested.
    pub path: VPath,
    /// The digest in LOWERCASE hex, or absent if there is none. Always
    /// lowercase, for the same reason as [`PlanHash`]: two spellings of the
    /// same hash comparing differently are a bug waiting to happen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// Why there is no digest. Absent when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub miss: Option<ChecksumMiss>,
}

/// Params of [`FS_CHECKSUM_REPORT`] (0.59.0, #311).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsChecksumReportParams {
    /// The Task whose report is requested (the one from
    /// [`FsTaskResult::task_id`] that [`FS_CHECKSUM`] returned).
    pub task_id: TaskId,
}

/// Params of [`FS_DIR_USAGE`] (0.75.0, phase 4).
///
/// ONE root and not several, unlike [`FsDirSizeParams`]: that one sums a
/// selection into a number, and this one describes what ONE directory is
/// made of. Mixing two roots would give a list of children from different
/// places with names that can repeat, which is not a map of anything.
///
/// ```
/// use norte_proto::methods::FsDirUsageParams;
/// // Omitted `depth` is ONE, which is what a map paints.
/// let p: FsDirUsageParams =
///     serde_json::from_str(r#"{"path":"file:///casa"}"#).expect("params");
/// assert_eq!(p.depth, 1);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsDirUsageParams {
    /// The directory being described.
    pub path: VPath,
    /// How many levels get opened. `1` — what a disk map paints — measures
    /// every child whole and does not go deeper.
    ///
    /// It is on the wire even though the server today only serves `1`
    /// because a two-level map is the first thing someone is going to ask
    /// for, and adding the field afterward would force distinguishing
    /// "did not send it" from "asked for one".
    ///
    /// **Contract**: `0` — describing zero levels is not a request — and
    /// anything past [`DIR_USAGE_MAX_DEPTH`] gets rejected with the
    /// `invalid-path` taxonomy (`-32000` with the category in `data`, like
    /// any application error of this protocol), **not** a bare `-32602`: a
    /// code with no category reaches the client as `internal`, meaning as a
    /// server failure and not as the caller's error.
    ///
    /// A VALID `depth` this server does not yet serve — today, anything
    /// greater than `1` — answers `unsupported`, and **is rejected instead
    /// of trimmed**: a server that silently trims leaves the client
    /// believing it has the levels it asked for.
    #[serde(default = "default_depth")]
    pub depth: u32,
}

/// The depth of an [`FS_DIR_USAGE`] that does not say it: one level.
const fn default_depth() -> u32 {
    1
}

/// Params of [`FS_DIR_USAGE_REPORT`] (0.75.0, phase 4).
///
/// ```
/// use norte_proto::methods::FsDirUsageReportParams;
/// let p: FsDirUsageReportParams =
///     serde_json::from_str(r#"{"task_id":9}"#).expect("params");
/// assert_eq!(p.task_id.get(), 9);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsDirUsageReportParams {
    /// The Task whose report is requested (the one from
    /// [`FsTaskResult::task_id`] that [`FS_DIR_USAGE`] returned).
    pub task_id: TaskId,
}

/// A measured child: what it is and how much it takes up (0.75.0, phase 4).
///
/// ```
/// use norte_proto::methods::DirUsageChild;
/// let c: DirUsageChild = serde_json::from_str(
///     r#"{"name":"caf%FF.txt","kind":"file","bytes":17,"entries":1}"#,
/// )
/// .expect("child");
/// // The name comes back as the BYTES that were on disk.
/// assert_eq!(c.name.as_bytes(), b"caf\xFF.txt");
/// // And a child that says nothing is a COMPLETE child: the lower bound
/// // is declared, not assumed.
/// assert!(!c.partial);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirUsageChild {
    /// Its BASE name, in bytes (rule 1): a name is not UTF-8, and whoever
    /// paints it masks it like any other.
    ///
    /// A [`Segment`] and not a `String` nor a path: it is exactly a name
    /// fragment, it already has its wire encoding, and a path here would be
    /// a second way to name a file — one that resolves against the
    /// requested directory, without going through where the others go.
    pub name: Segment,
    /// What class of node it is, so the map paints it as what it is.
    pub kind: EntryKind,
    /// How much it takes up, counting what could be read under it.
    pub bytes: u64,
    /// How many entries it has inside, itself included.
    pub entries: u64,
    /// The above is a LOWER BOUND: there was something under this child
    /// that did not let itself be read.
    ///
    /// Per CHILD and not per report, which is the difference between being
    /// able to paint it and not: a map marks the rectangle that is not
    /// complete; a global flag can only turn off the whole map. It is the
    /// same criterion as [`ChecksumEntry::miss`], which is also per entry.
    ///
    /// `default` so adding fields here stays additive.
    #[serde(default)]
    pub partial: bool,
}

/// Result of [`FS_DIR_USAGE_REPORT`] (0.75.0, phase 4): what has been
/// measured so far.
///
/// It is a SNAPSHOT, like [`FS_CHECKSUM_REPORT`]'s: partial while the Task
/// runs — which is what makes asking for it useful — and definitive once it
/// finishes. The order is the directory's LISTING order, not the
/// completion order: a map that reorders itself just changes shape between
/// two views of the same thing.
///
/// ```
/// use norte_proto::methods::FsDirUsageReportResult;
/// // A fragment: the Task stopped while listing the root.
/// let r: FsDirUsageReportResult = serde_json::from_str(
///     r#"{"children":[],"total_bytes":0,"total_entries":0,
///         "pending":0,"listed":false,"omitted":0}"#,
/// )
/// .expect("report");
/// // `pending: 0` does NOT mean "finished" here: without `listed`, nobody
/// // knows how many children there were.
/// assert!(!r.listed);
/// assert_eq!(r.pending, 0);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsDirUsageReportResult {
    /// One child per directory entry, already measured.
    pub children: Vec<DirUsageChild>,
    /// The sum of the measured children.
    pub total_bytes: u64,
    /// The entries counted under them.
    pub total_entries: u64,
    /// How many children remain to be measured. Zero with a `Completed`
    /// Task; on a cancelled or failed one it stays at what was missing.
    ///
    /// **Only means something with [`Self::listed`] `true`.** Unlike
    /// [`FsChecksumReportResult`]'s, whose denominator is the paths the
    /// client sent, here it is not known how many children there are until
    /// the root's listing finishes: a Task cancelled while listing reports
    /// `0` over a handful of children, and without `listed` that reads as a
    /// complete map.
    pub pending: u64,
    /// The root's listing FINISHED, so how many children there are is
    /// already known.
    ///
    /// At `false` the report is a fragment — it was cancelled while
    /// listing, or the root did not even let itself be opened — and
    /// neither [`Self::pending`] nor the totals say anything about what is
    /// missing.
    pub listed: bool,
    /// Children that EXIST and are not in the list, because they did not
    /// fit ([`DIR_USAGE_MAX_CHILDREN`]).
    ///
    /// The totals DO count them, so a map can paint the rest as one more
    /// rectangle: what is lost is their name, not their size. Zero is the
    /// normal case.
    pub omitted: u64,
}

/// Result of [`FS_CHECKSUM_REPORT`] (0.59.0, #311): what has been computed
/// so far.
///
/// The order is the REQUEST's, not the completion order: a report that
/// reorders itself cannot be compared against the list one sent.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsChecksumReportResult {
    /// One entry per already-resolved path, in the order they were requested.
    pub entries: Vec<ChecksumEntry>,
    /// With which function these digests were computed.
    ///
    /// It goes in the REPORT and not only in the params because the report
    /// can be requested without having sent the request: `task.list` shows
    /// other people's Tasks. The day `sha512` exists, a reader assuming
    /// sha256 by default would paint digests of something else without
    /// saying so — the wrong answer from the one tool whose job is to
    /// check.
    pub algo: ChecksumAlgo,
    /// How many paths remain to be resolved.
    ///
    /// Drops to zero as the batch advances, and with a `Completed` Task it
    /// is zero. It is **not** zero on a terminal Task that got CANCELLED or
    /// failed: there it stays at what was missing, and that is exactly the
    /// signal that the report is halfway done. A reader that treats
    /// `pending > 0` as "not yet" and keeps polling would never stop; what
    /// distinguishes "this is everything" from "this is what I have so
    /// far" is the two things together, the Task's state and this number.
    pub pending: u64,
}

/// Params of [`FS_RENAME_BATCH_REPORT`] (0.36.0).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsRenameBatchReportParams {
    /// The batch's Task whose report is requested (the one from
    /// [`FsTaskResult::task_id`] that [`FS_RENAME_BATCH`] returned).
    pub task_id: TaskId,
}

/// Result of [`FS_RENAME_BATCH_REPORT`] (0.36.0): what the batch did.
///
/// A clean run leaves `applied` == the plan's step count and everything
/// else at zero/absent. **Any other shape is the executor telling the
/// truth about a directory it could not leave as it found it** — read it
/// even if the Task failed, and especially when it says `cancelled`.
///
/// ```
/// use norte_proto::methods::FsRenameBatchReportResult;
/// let r = FsRenameBatchReportResult {
///     applied: 3,
///     rolled_back: 0,
///     failed_pair: None,
///     stuck: None,
///     uncertain: None,
///     compensations_lost: 0,
/// };
/// let json = serde_json::to_value(&r).expect("json");
/// // The absent gets OMITTED: a clean batch does not send three `null`s.
/// assert_eq!(json.get("stuck"), None);
/// assert_eq!(json["applied"], serde_json::json!(3));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsRenameBatchReportResult {
    /// Steps applied AND journaled.
    pub applied: u64,
    /// Steps the rollback managed to return.
    pub rolled_back: u64,
    /// The requested pair whose step failed, if any failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_pair: Option<u32>,
    /// The rollback refused HERE and stopped: the directory is
    /// half-renamed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck: Option<RenameStuckStep>,
    /// A step whose provider reported failure and that could not
    /// afterward be PROBED, so whether it took effect or not is unknown.
    /// Neither of the two was assumed: `to` is where to look.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertain: Option<RenameStuckStep>,
    /// Reversals that were APPLIED but whose compensating entry could not
    /// be written. The tree came back; the journal still says the direct
    /// rename is in effect, so a later `policy.undo_session` will reach
    /// that entry and get BLOCKED there. A value other than zero is the
    /// only warning of that.
    #[serde(default)]
    pub compensations_lost: u64,
}

/// A client's identity (goes in [`InitializeParams`]).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// Frontend name (`norte-tui`, `norte-cli`, a third party…).
    pub name: String,
    /// Frontend version (informational, never compared).
    pub version: String,
}

/// The server's identity (goes in [`InitializeResult`]).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Server name (`norte-core`).
    pub name: String,
    /// Daemon binary version (informational).
    pub version: String,
}

/// Params of [`INITIALIZE`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeParams {
    /// Who is connecting.
    pub client_info: ClientInfo,
    /// Client's protocol version; incompatible = error and close.
    pub protocol_version: String,
    /// Encodings the client can speak, by preference. Empty or absent =
    /// implicit `["json"]` (the only one in M2, kickoff decision 4:
    /// negotiated-but-JSON-only).
    #[serde(default)]
    pub encodings: Vec<String>,
    /// If present, the connection acts as an AGENT SESSION with this id:
    /// its mutations are evaluated by the policy engine (M3-3). Absent =
    /// human frontend (`User`, no sandbox). The server binds the actor to
    /// the connection; a client cannot declare itself `User` any other way.
    ///
    /// The id is validated server-side fail-closed: 1..=64 chars of
    /// `[A-Za-z0-9._-]`, otherwise `INVALID_PARAMS` — it travels to the
    /// journal, logs and frontends' approval modals, it must never be an
    /// injection vector (control chars/bidi) chosen by the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<String>,
}

/// Result of [`INITIALIZE`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeResult {
    /// Who is answering.
    pub server_info: ServerInfo,
    /// The core's protocol version.
    pub protocol_version: String,
    /// Accepted encodings (today always `["json"]`).
    pub encodings: Vec<String>,
}

/// Params of [`DAEMON_GOING_AWAY`].
///
/// ```
/// use norte_proto::methods::DaemonGoingAway;
/// let n = DaemonGoingAway { reconnect: true };
/// let j = serde_json::to_value(&n).expect("json");
/// assert_eq!(j["reconnect"], serde_json::json!(true));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonGoingAway {
    /// `true` = a handoff is coming; reconnect, and start it if it is not
    /// there. `false` = this daemon stops and stays stopped.
    ///
    /// The `false` is not decoration: sending it on an ordinary stop is
    /// what lets a frontend say "the daemon stopped" instead of "the
    /// connection dropped", which for whoever reads it are not the same
    /// thing.
    pub reconnect: bool,
}

/// What class of shutdown this is (0.46.0).
///
/// ORTHOGONAL to [`DaemonShutdownParams::graceful`], which decides what
/// happens to live tasks. This one decides who comes next.
///
/// **No `#[serde(other)]`, unlike almost all of this wire.** The rest of
/// the enums degrade in the face of an unknown value because
/// misinterpreting them costs a feature; misinterpreting this one shuts
/// down a daemon in a way the caller did not ask for. Same asymmetry as
/// the mutation policies (ADR 0005).
///
/// ```
/// use norte_proto::methods::ShutdownMode;
/// assert_eq!(ShutdownMode::default(), ShutdownMode::Stop);
/// assert!(serde_json::from_str::<ShutdownMode>(r#""made_up""#).is_err());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownMode {
    /// Stops and stays stopped. The usual behavior.
    #[default]
    Stop,
    /// A handoff is coming: clients must come back, and start it if it is
    /// not there.
    Handover,
}

impl ShutdownMode {
    /// Is it the usual stop? Used by
    /// [`DaemonShutdownParams::mode`]'s `skip_serializing_if`.
    #[must_use]
    pub const fn is_stop(&self) -> bool {
        matches!(self, Self::Stop)
    }
}

/// Params of [`DAEMON_SHUTDOWN`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonShutdownParams {
    /// `true` (default): finish live tasks before exiting. `false`: cancel
    /// them first (clean state still guaranteed).
    #[serde(default = "default_graceful")]
    pub graceful: bool,
    /// Stop or handover (0.46.0). Default [`ShutdownMode::Stop`], which is
    /// what this method did before the field existed.
    ///
    /// Not serialized when it is `Stop`: the message this client sends for
    /// an ordinary stop stays BYTE FOR BYTE 0.45's, so backward
    /// compatibility does not depend on the other side ignoring fields it
    /// does not know — it depends on there being no field.
    #[serde(default, skip_serializing_if = "ShutdownMode::is_stop")]
    pub mode: ShutdownMode,
}

impl Default for DaemonShutdownParams {
    fn default() -> Self {
        Self {
            graceful: true,
            mode: ShutdownMode::Stop,
        }
    }
}

fn default_graceful() -> bool {
    true
}

/// Result of [`DAEMON_SHUTDOWN`]: empty object, reserved for extension.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonShutdownResult {}

/// Params of [`TASK_LIST`]: empty object, reserved for extension (filters
/// by state/kind will arrive here as optional fields).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskListParams {}

/// Result of [`TASK_LIST`].
///
/// ```
/// use norte_proto::methods::TaskListResult;
/// let r: TaskListResult = serde_json::from_str(r#"{"tasks":[]}"#).unwrap();
/// assert!(r.tasks.is_empty());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskListResult {
    /// Snapshots of live tasks + the recent outcomes the server retains
    /// (best effort; see [`TASK_LIST`]). May repeat `task_id` — the
    /// receiver deduplicates.
    pub tasks: Vec<crate::TaskProgress>,
}

/// Params of [`FS_READ`].
///
/// ```
/// use norte_proto::methods::FsReadParams;
/// use norte_proto::VPath;
/// let p = FsReadParams { path: VPath::parse("file:///x").unwrap(), range: None };
/// // The canonical emitter writes `range: null` explicitly (ADR 0004).
/// assert!(serde_json::to_string(&p).unwrap().contains(r#""range":null"#));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsReadParams {
    /// File to read.
    pub path: VPath,
    /// Requested range; absent/`null` = from 0, server's ceiling.
    #[serde(default)]
    pub range: Option<crate::ByteRange>,
}

/// Result of [`FS_READ`].
///
/// ```
/// use norte_proto::methods::FsReadResult;
/// let r: FsReadResult = serde_json::from_str(r#"{"content_b64":"aGk=","eof":true}"#).unwrap();
/// assert!(r.eof);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsReadResult {
    /// Bytes of the chunk, in standard base64 (a file's bytes are not
    /// text: JSON cannot carry them raw).
    pub content_b64: String,
    /// `true` if the chunk ends AT the end of the file.
    pub eof: bool,
}

/// Params of [`FS_CAPABILITIES`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsCapabilitiesParams {
    /// The LOCATION to query. A file is answered for by the directory
    /// that contains it (ADR 0054).
    pub path: VPath,
}

/// Result of [`FS_CAPABILITIES`].
///
/// ```
/// use norte_proto::methods::FsCapabilitiesResult;
/// // A catalog with a malformed id does NOT reach the vector: it is
/// // discarded on decode, without error (see [`FsCapabilitiesResult::attrs`]).
/// let wire = r#"{
///     "capabilities": {"flags": "", "max_path": null},
///     "attrs": [{"id": "MODE", "label": "Mode", "type": "uint", "hint": "mode"}]
/// }"#;
/// let caps: FsCapabilitiesResult = serde_json::from_str(wire).unwrap();
/// assert!(caps.attrs.is_empty());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsCapabilitiesResult {
    /// Capabilities of the queried location (ADR 0054): the provider's
    /// declaration, refined with whatever can be found out about THAT
    /// directory.
    pub capabilities: crate::Capabilities,
    /// Provider attributes this provider offers (0.30.0, ADR 0039): the
    /// discovery half of [`Entry::attrs`](crate::Entry::attrs). Empty (the
    /// default, and the only possibility for a 0.29 peer) is omitted from the
    /// wire entirely, so a response from a provider that publishes none is
    /// byte-identical to 0.29's.
    ///
    /// [`AttrInfo::label`](crate::attrs::AttrInfo::label) is THIRD-PARTY text
    /// — a frontend masks and clamps it exactly as it does a plugin's column
    /// header. The ORDER is the provider's and it is meaningful: a column
    /// picker paints the catalog in it, so it is never sorted here.
    ///
    /// # Rules applied ON DECODE
    ///
    /// This is RECEIVED data, so the same shape of filter that guards
    /// [`Entry::attrs`](crate::Entry::attrs) guards it: decoding runs
    /// [`sanitize_catalog`](crate::attrs::sanitize_catalog), whose rules are
    /// documented there in full and NONE of which is ever an error —
    ///
    /// 1. an [`AttrInfo`](crate::attrs::AttrInfo) whose `id` is not
    ///    well-formed ([`is_valid_attr_id`](crate::attrs::is_valid_attr_id))
    ///    is DROPPED. An advertised id becomes a requested id, a configuration
    ///    id and a map lookup downstream, so `MODE` or `../etc/passwd` must
    ///    not survive the wire — and a caller must not have to remember to
    ///    check;
    /// 2. a REPEATED id is dropped, FIRST WINS — the opposite of
    ///    [`Entry::attrs`](crate::Entry::attrs)'s last-wins, because this is
    ///    an ordered list the provider ranked (where "first" means something)
    ///    and that is an unordered JSON object (where it does not). Keeping
    ///    both would let two consumers render the same bytes differently, one
    ///    folding into a map and one using `find()`;
    /// 3. an over-long `label` is CLAMPED to
    ///    [`ATTR_LABEL_MAX`](crate::attrs::ATTR_LABEL_MAX) bytes on a char
    ///    boundary; the descriptor survives, since the id is what a client
    ///    acts on;
    /// 4. the vector is bounded at
    ///    [`ATTRS_MAX_ADVERTISED`](crate::attrs::ATTRS_MAX_ADVERTISED)
    ///    descriptors, keeping the FIRST in wire order — rules 1 and 2 run
    ///    before a slot is taken, so rejects and duplicates never starve a
    ///    legitimate later attribute. A fat catalog is a buggy provider, not a
    ///    broken peer, so it truncates rather than failing the call: the same
    ///    spirit as an unknown requested id coming back absent.
    ///
    /// Decoding additionally stops EXAMINING elements past
    /// [`ATTRS_MAX_CATALOG_SCAN`](crate::attrs::ATTRS_MAX_CATALOG_SCAN) and
    /// drains the rest unmaterialised, so a catalog padded with rejects costs
    /// bounded work rather than unbounded work for an empty result.
    ///
    /// Those rules are NOT the deserialiser's alone: the field is an
    /// [`AttrCatalog`](crate::attrs::AttrCatalog), whose only constructor runs
    /// `sanitize_catalog` and whose contents are private. That matters because
    /// an EMBEDDED backend — the default TUI/CLI configuration, no daemon in
    /// between — never crosses the deserialisation boundary, so in block 2 a
    /// catalog from a WASM provider plugin (untrusted by the threat model)
    /// would otherwise reach a frontend unfiltered whenever someone forgot the
    /// call. The wire shape is unchanged: a plain array of `AttrInfo`.
    ///
    /// A descriptor that is not a well-formed `AttrInfo` at all (a missing
    /// `label`, `attrs` that is not a list) IS a hard error: that is serde's
    /// decision, and a peer that sends one is broken rather than newer — an
    /// unknown `type` or `hint` from a NEWER peer already degrades to its
    /// `Unknown` variant instead.
    ///
    /// # Unlike `Entry::attrs`, this cannot carry a producer's bug
    ///
    /// [`Entry::attrs`](crate::Entry::attrs) is a public map with no
    /// constructor, so an entry built in-process with an invalid id emits it
    /// and decodes back different — deliberately, so a producer's bug stays
    /// visible at the boundary that validates it. A catalog has no such hole:
    /// it cannot be built dirty in the first place, so serialisation has
    /// nothing to launder. The asymmetry is on purpose — an advertised id
    /// becomes a REQUESTED id, a configuration id and a map lookup downstream,
    /// which is a longer blast radius than one entry's cell.
    #[serde(default, skip_serializing_if = "crate::attrs::AttrCatalog::is_empty")]
    #[cfg_attr(
        feature = "schema",
        schemars(
            with = "Vec<crate::attrs::AttrInfo>",
            extend("maxItems" = crate::attrs::ATTRS_MAX_ADVERTISED)
        )
    )]
    pub attrs: crate::attrs::AttrCatalog,
}

/// What kind of volume a [`Volume`] is, over the wire (0.37.0, #131). Mirrors
/// `norte_core::volumes::VolumeKind` field for field, but this crate cannot
/// depend on `norte-core` (the dependency runs the other way), so the two
/// stay in sync by convention plus the daemon-side mapping test, not by a
/// shared type.
///
/// `#[serde(other)]` on `Unknown` is the same forward-compat shape
/// [`EntryKind::Other`] and `TaskKind::Unknown` already use: a kind this
/// client has never heard of degrades to `Unknown` on decode instead of
/// failing the whole `host.volumes` response.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeKind {
    /// A local, non-removable disk.
    Fixed,
    /// A local disk the OS considers removable (USB, SD, …).
    Removable,
    /// A network filesystem (NFS, CIFS/SMB, sshfs, …).
    Network,
    /// A synthetic/virtual filesystem (`proc`, `tmpfs`, …), hidden from the
    /// picker unless it asks for everything.
    Pseudo,
    /// The platform could not tell, or this decoder does not recognize a
    /// kind a newer peer sent (`#[serde(other)]`).
    #[serde(other)]
    Unknown,
}

/// (De)serializes [`Volume::label`] as base64 text, or an absent key —
/// mirroring [`crate::attrs::AttrValue::Bytes`]'s wire shape (§ its
/// rustdoc) rather than inventing a third byte-carrying representation next
/// to it and [`VPath`]. `VPath` does not fit here: it is a scheme +
/// authority + segment structure for a PATH, and a label is a flat blob with
/// none of that shape to preserve.
///
/// Unlike `AttrValue::Bytes`, malformed base64 does not need the "whole cell
/// degrades to `Unknown`" ceremony ADR 0039 built for third-party plugin
/// data: a `Volume` is produced by the host service, not a WASM guest, and a
/// broken `label` is cheaply dropped to `None` — the rest of the `Volume`
/// (`mount` above all) is still perfectly usable, so losing the WHOLE entry
/// over one bad field would be a worse failure than the one it avoids.
///
/// A real ext4/vfat/NTFS label is at most a few dozen bytes, but nothing
/// upstream enforces that on the wire — so a decoded payload OVER
/// [`crate::attrs::ATTR_BYTES_MAX`] bytes degrades exactly like an
/// undecodable one (encoding-auditor MINOR, V3.5 review): the same cap
/// `AttrValue::Bytes` already publishes, reused rather than a second
/// bespoke number for what is the same class of payload.
mod label_wire {
    use serde::{Deserialize as _, Deserializer, Serializer};

    use crate::attrs::{ATTR_BYTES_MAX, decode_bytes_b64_lenient, encode_bytes_b64};

    // `ref_option` (pedantic) wants `Option<&T>` here, but serde's `with =`
    // codegen calls this with `&self.label` — the field's actual type,
    // `&Option<Vec<u8>>` — not something this function gets to choose.
    #[expect(
        clippy::ref_option,
        reason = "serde `with` fixes the signature to `&Option<Vec<u8>>`"
    )]
    pub(super) fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(bytes) => s.serialize_str(&encode_bytes_b64(bytes)),
            None => s.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        let raw = Option::<String>::deserialize(d)?;
        // Undecodable OR oversized base64 costs this ONE field, not the
        // `Volume` it is on — see the module's rustdoc.
        Ok(raw.and_then(|s| {
            decode_bytes_b64_lenient(&s).filter(|bytes| bytes.len() <= ATTR_BYTES_MAX)
        }))
    }
}

/// One volume the host has mounted, over the wire (0.37.0, #131; `label`'s
/// current byte shape is 0.38.0, see its own doc).
///
/// # Non-UTF-8 mount points and labels
/// `mount` is a [`VPath`], never a `String`: rule 1 requires a mount point's
/// bytes to survive the wire exactly, and a `String` cannot hold bytes that
/// are not valid UTF-8 (`/proc/mounts` places no such restriction on what a
/// filesystem may be mounted at). [`Volume::label`] carries the same hazard
/// for a different reason — see its own rustdoc for the per-platform
/// breakdown, Windows above all.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    /// Mount point. A [`VPath`] — see the type's rustdoc for why this is
    /// never a `String`.
    pub mount: VPath,
    /// What the OS or the filesystem calls it, when it says — raw bytes,
    /// base64 on the wire (`label_wire`, this module), never a `String`. No platform
    /// this crate targets promises the label is UTF-8, and Windows'
    /// `GetVolumeInformationW` returns UTF-16 rather than bytes at all — see
    /// `norte_core::volumes::Volume::label`'s rustdoc for the full
    /// per-platform breakdown (that crate cannot be linked from here, the
    /// dependency runs the other way, hence the plain-text pointer). In
    /// short: Linux is `None` today (`/proc/mounts` carries no label);
    /// macOS and Windows are V4 and unverified, and Windows crosses as
    /// WTF-8 — the same encoding CONVENTION `norte_vfs_local`'s (private)
    /// `native_path`/`wtf8` modules already apply to Windows path segments,
    /// not literally reusable code (this crate cannot depend on
    /// `norte-vfs-local` either) — so an unpaired surrogate a FAT/NTFS
    /// label can legally contain survives instead of being replaced with
    /// `U+FFFD` before rule 1 gets a say.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "label_wire")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Option<String>", extend("contentEncoding" = "base64"))
    )]
    pub label: Option<Vec<u8>>,
    /// `ext4`, `apfs`, `ntfs`, `nfs4`… as the platform spells it.
    pub fs_type: String,
    /// What kind of volume this is, so far as the platform can tell.
    pub kind: VolumeKind,
    /// `None` when the filesystem did not answer its space query in time —
    /// never a zero standing in for "unknown" (design §A: a `0` here would
    /// read as "full").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    /// `None` when the filesystem did not answer its space query in time —
    /// see [`Volume::total_bytes`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_bytes: Option<u64>,
    /// Whether the mount is read-only.
    pub read_only: bool,
}

/// Params of [`HOST_VOLUMES`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostVolumesParams {
    /// The picker's "show everything" toggle: with this `false` (the
    /// default), a [`Volume`] classified [`VolumeKind::Pseudo`] is left out.
    #[serde(default)]
    pub include_pseudo: bool,
}

/// Result of [`HOST_VOLUMES`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostVolumesResult {
    /// The host's volumes, in no particular guaranteed order.
    pub volumes: Vec<Volume>,
}

/// ONE named connection among those the daemon has configured (0.56.0,
/// #264).
///
/// ```
/// use norte_proto::methods::ConnectionEntry;
/// let e: ConnectionEntry = serde_json::from_str(
///     r#"{"name": "work", "url": "sftp://oscar@server.example/data"}"#,
/// )
/// .unwrap();
/// assert_eq!(e.name, "work");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionEntry {
    /// The name the user wrote it under in `connections.toml`.
    ///
    /// It is text the human chose, not a system identifier: whoever
    /// paints it masks it like any other name.
    pub name: String,
    /// The URL as it is in the file.
    ///
    /// May carry a user (`sftp://oscar@host/…`), which is normal.
    /// **Never a password**: credentials are REFERENCED (ADR 0015) and
    /// this list does not resolve them. Whoever paints it treats it as a
    /// wire authority, with the same care as a degraded-session notice —
    /// a host can be named `bank.example@evil.example`.
    pub url: String,
}

/// Result of [`CONNECTION_LIST`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionListResult {
    /// The connections, in alphabetical order by name.
    ///
    /// Empty = none configured, which is normal on day one. A file whose
    /// SYNTAX does not parse IS an error: saying "you have none" when
    /// what happened is there is one extra comma would be lying about
    /// what the user wrote.
    pub connections: Vec<ConnectionEntry>,
    /// The ones the daemon could not read, with the reason (0.84.0, #365).
    ///
    /// They go SEPARATELY and do not sneak in among the good ones: an
    /// unusable entry is not a connection one can go to, and offering it
    /// in a picker would be offering a dead button. But it does not
    /// disappear either — which is what happened before 0.84.0, when ONE
    /// bad entry made the whole call fail and the reader lost the list of
    /// all its connections with an error that named none of them.
    ///
    /// Empty in the normal case. An N-1 client does not see it and still
    /// sees the good ones, which is exactly the improvement.
    #[serde(default)]
    pub unusable: Vec<ConnectionProblem>,
}

/// A `connections.toml` entry the daemon could not read (0.84.0, #365).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionProblem {
    /// Its name, as written in the file. It is what the reader has to go
    /// look for, so without it the notice is useless.
    pub name: String,
    /// What is wrong with it, in the parser's language. It is not pretty
    /// and it is ACTIONABLE, which is what is needed: it says which field
    /// is extra or missing.
    ///
    /// **Never a secret.** What fails is the SHAPE of the entry, and a
    /// `ConnectionSpec` references its credentials instead of storing
    /// them (ADR 0015) — but whoever paints it treats it as wire text,
    /// same as a URL: someone wrote the file.
    pub reason: String,
}

/// What a comparison concluded about ONE pair (0.39.0, ADR 0048).
///
/// The verdict says WHAT, [`CompareCriterion`] says WHICH RUNG decided it and
/// [`CompareConfidence`] says WHAT THAT IS WORTH. The three travel together on
/// every [`CompareRow`] precisely because `Same` alone is not an answer: a
/// `Same` earned by comparing bytes and a `Same` earned by two dates that are
/// two seconds apart are different facts, and a client that cannot tell them
/// apart cannot tell the user either.
///
/// `#[serde(other)]` on `Unknown` is the forward-compat shape
/// [`EntryKind::Other`] and [`VolumeKind::Unknown`]
/// already use: a verdict an N+1 daemon adds degrades to `Unknown` on decode
/// instead of failing the whole batch of rows.
///
/// ```
/// use norte_proto::methods::CompareVerdict;
/// assert_eq!(
///     serde_json::to_string(&CompareVerdict::OnlyLeft).expect("json"),
///     r#""only_left""#
/// );
/// // A verdict from a newer daemon degrades; it does NOT throw out the
/// // whole batch.
/// let future: CompareVerdict = serde_json::from_str(r#""conflicted""#).expect("degrades");
/// assert_eq!(future, CompareVerdict::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum CompareVerdict {
    /// Both sides are held equal under the criterion that decided it.
    Same,
    /// Both sides differ under the criterion that decided it.
    Different,
    /// Only exists on the left. `right` is `None` (see
    /// [`CompareRow::sides_are_consistent`]).
    OnlyLeft,
    /// Only exists on the right. `left` is `None`.
    OnlyRight,
    /// Same name, different [`EntryKind`]: a file against a directory is
    /// not a content difference, it is something else.
    TypeMismatch,
    /// Two entries of the SAME side collapse onto the same pairing key
    /// (`README`/`readme` against APFS, NFC/NFD on ext4): they are not
    /// paired, they are REPORTED — with [`CompareRow::reason`] populated.
    /// It is exactly the collision a later synchronization has to see
    /// BEFORE writing anything.
    ///
    /// # Row shape (normative)
    /// The collision belongs to ONE side, so the row does too: **one row
    /// per involved entry** is emitted, with that entry in ITS side's
    /// field, the other side at `None`, and [`CompareRow::side`] naming
    /// where it happened. Two names that collapse are TWO rows, never one
    /// that joins them and never a deduplicated one — losing a name here is
    /// losing exactly the file a sync plan was going to write to.
    ///
    /// Nothing on the wire prevents it (`sides_are_consistent` exempts this
    /// verdict: there is no sides rule a client could check, and asserting
    /// one would make it distrust legitimate rows from an N+1 daemon), so
    /// it is a contract of whoever produces the rows and is frozen in the
    /// `ambiguous_*` goldens of `compare_row.json`. If someday it becomes
    /// necessary to show the two colliding entries in ONE row, the wire
    /// needs a new field: the two that exist are SIDES, not members of a
    /// collision (MAJOR finding from protocol-guardian, C1 review).
    Ambiguous,
    /// The walk could not answer for this entry (unreadable listing,
    /// directory over [`COMPARE_MAX_DIR_ENTRIES`], read failed mid-hash).
    /// Carries [`CompareRow::reason`] and, when it applies to one side,
    /// [`CompareRow::side`]. Does NOT end the Task.
    Error,
    /// Verdict this decoder does not know (`#[serde(other)]`): an N+1
    /// daemon emitted it, this client paints it as "I don't know what this
    /// says".
    #[serde(other)]
    Unknown,
}

/// Which RUNG of the cascade decided a row (0.39.0, ADR 0048).
///
/// The cascade goes from cheap to expensive and stops at the first rung
/// that decides, so this field is also "how far it had to go". Without it,
/// `Different` does not say whether a size or 40 GB of bytes got compared.
///
/// ```
/// use norte_proto::methods::CompareCriterion;
/// assert_eq!(
///     serde_json::to_string(&CompareCriterion::LinkTarget).expect("json"),
///     r#""link_target""#
/// );
/// let future: CompareCriterion = serde_json::from_str(r#""etag""#).expect("degrades");
/// assert_eq!(future, CompareCriterion::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum CompareCriterion {
    /// One side does not exist: there is nothing else to compare.
    ///
    /// It is ALSO the criterion of rows no rung decided — an unreadable
    /// listing, a directory over [`COMPARE_MAX_DIR_ENTRIES`], a pairing
    /// collision — by convention and not because it is true: the field is
    /// not optional (a nullable key on every row of a path carrying
    /// millions is not worth paying for a value nobody branches on) and
    /// `Unknown` is the decode fallback, which the core never emits. On
    /// those rows what informs is `verdict: error`/`ambiguous` plus
    /// [`CompareRow::reason`]; the criterion is not read. See ADR 0048,
    /// negative consequences.
    Presence,
    /// The [`EntryKind`]s differ.
    Kind,
    /// Symlink targets compared AS BYTES (never followed: with no
    /// following there is no need to detect cycles, and a link whose
    /// target changed is a real difference).
    LinkTarget,
    /// Sizes in bytes.
    Size,
    /// Modification dates, under the request's tolerance
    /// ([`FsCompareParams::mtime_tolerance_ms`]).
    Mtime,
    /// Streaming sha256 of both sides. Only runs if the caller asked for it
    /// and only reaches pairs the cheap rungs ruled equal.
    Hash,
    /// A criterion this decoder does not know (`#[serde(other)]`).
    #[serde(other)]
    Unknown,
}

/// How much a row's verdict is worth (0.39.0, ADR 0048).
///
/// The item's central vocabulary: a comparison DECLARES what its criterion
/// has earned. A different size PROVES different bytes (`Certain`); a
/// different date only SUGGESTS it (`Probable`); a provider that cannot
/// answer leaves `Unknown`, which is an honest answer and not a failure.
///
/// # Why the fallback here is called `Unrecognised`
/// In [`CompareVerdict`] and [`CompareCriterion`] the `#[serde(other)]`
/// fallback is called `Unknown`, the house convention. Here `Unknown` is
/// already a VALUE with meaning — "the provider cannot say" — and "a newer
/// peer said something I do not know" is a DIFFERENT fact. Sharing the name
/// would turn an honest answer into a protocol mismatch, and vice versa.
///
/// ```
/// use norte_proto::methods::CompareConfidence;
/// // `unknown` is a REAL value of the vocabulary...
/// let honest: CompareConfidence = serde_json::from_str(r#""unknown""#).expect("real value");
/// assert_eq!(honest, CompareConfidence::Unknown);
/// // ...and the forward-compat fallback is ANOTHER one.
/// let future: CompareConfidence = serde_json::from_str(r#""quantum""#).expect("degrades");
/// assert_eq!(future, CompareConfidence::Unrecognised);
/// assert_ne!(honest, future);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum CompareConfidence {
    /// The criterion PROVES its verdict (presence, kind, different size,
    /// symlink target, hash).
    Certain,
    /// The criterion SUGGESTS its verdict without proving it (mtime: two
    /// files with the same date can have different bytes, and vice versa).
    Probable,
    /// The provider cannot say: there is no size, no reliable date (a
    /// compressed archive, an object store whose `ETag` is only sometimes a
    /// hash).
    /// It is an ANSWER, not an error.
    Unknown,
    /// A confidence this decoder does not know (`#[serde(other)]`). It is
    /// NOT [`CompareConfidence::Unknown`] — see the section above.
    #[serde(other)]
    Unrecognised,
}

/// Why a row is [`CompareVerdict::Ambiguous`] or [`CompareVerdict::Error`]
/// (0.39.0, ADR 0048). CLOSED vocabulary: the core never invents a class.
///
/// ```
/// use norte_proto::methods::CompareReason;
/// assert_eq!(
///     serde_json::to_string(&CompareReason::DirTooLarge).expect("json"),
///     r#""dir_too_large""#
/// );
/// let future: CompareReason = serde_json::from_str(r#""solar_flare""#).expect("degrades");
/// assert_eq!(future, CompareReason::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum CompareReason {
    /// Two names on the same side that only differ in case collapse
    /// because the OTHER side does not distinguish case.
    CaseFold,
    /// Two names on the same side that collapse when normalized to NFC
    /// (the classic macOS NFD alongside its NFC twin).
    Normalization,
    /// Could not READ what was needed to compare (permissions, I/O): a
    /// directory that would not list, or an entry that would not `stat`
    /// when the size or date rung needed its data. See
    /// [`CompareRow::side`], which names the side that failed.
    ///
    /// It is not a confidence `Unknown`: `Unknown` is "the provider cannot
    /// answer this question" and travels with a `Same` verdict; this is
    /// "could not even ask", and travels with [`CompareVerdict::Error`].
    Unreadable,
    /// The directory exceeds [`COMPARE_MAX_DIR_ENTRIES`] entries.
    DirTooLarge,
    /// A read failed mid-way through the hash rung. See [`CompareRow::side`].
    ReadFailed,
    /// A reason this decoder does not know (`#[serde(other)]`).
    #[serde(other)]
    Unknown,
}

/// Under which transform two names that are NOT the same bytes paired
/// (0.42.0, #152).
///
/// The pairing key case-folds and normalizes to NFC, and **neither one is
/// injective**: `README` and `readme` pair because one side cannot sustain
/// both spellings, and NFC `café` and NFD `café` because they are the SAME
/// text written two ways. Both things are the wanted behavior. What was not
/// visible is that the resulting row — a perfectly normal
/// [`CompareVerdict::Same`] or [`CompareVerdict::Different`] — did not say
/// that its two halves are not the same bytes, and
/// [`CompareRow::reason_is_consistent`] forbids a [`CompareReason`] outside
/// [`CompareVerdict::Ambiguous`]/[`CompareVerdict::Error`], so there was
/// nowhere to say it.
///
/// [`PairTransform::NormalizationSingleton`] is the case this field exists
/// for. NFC has SINGLETON decompositions — U+212A KELVIN SIGN normalizes to
/// `K`, U+2126 OHM SIGN to U+03A9 — so two files that coexist on ext4, with
/// no case folding involved, and that a reader reads as DISTINCT
/// characters, pair and compare as if they were one. With no mark on the
/// wire, a sync reads that row as "update the right one with the left one"
/// and writes over a file that has nothing to do with it.
///
/// **Separating the singleton from the rest is this vocabulary's whole
/// value.** A consumer that only knew "these two names differ in bytes"
/// would have to choose between trusting every normalization pairing —
/// which is the bug — or rejecting them all, and that breaks the
/// macOS↔Linux case the key was designed for.
///
/// Daemon→client: `#[serde(other)]`, like all of ADR 0048's vocabulary.
///
/// ```
/// use norte_proto::methods::PairTransform;
/// assert_eq!(
///     serde_json::to_string(&PairTransform::NormalizationSingleton).expect("json"),
///     r#""normalization_singleton""#
/// );
/// // A transform from an N+1 daemon degrades; it does NOT throw out the
/// // batch of rows.
/// let future: PairTransform = serde_json::from_str(r#""transliteration""#).expect("degrades");
/// assert_eq!(future, PairTransform::Unknown);
/// assert!(!PairTransform::Unknown.names_one_text());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum PairTransform {
    /// **Without case folding they do NOT pair**: the fold was needed,
    /// because one of the two sides does not distinguish case (see
    /// `norte_compare::Sides`).
    ///
    /// It does not say "they differ ONLY in case": a pair that is also in
    /// different Unicode spellings — precomposed `CAFÉ` against decomposed
    /// `café` — answers this too, because what joins them is the fold.
    /// What it does promise is [`PairTransform::names_one_text`].
    ///
    /// It is not a warning: on the side that does not distinguish case the
    /// two names CANNOT coexist, so pairing them is exactly correct. It
    /// travels so a painter can explain why the row shows two spellings.
    CaseFold,
    /// Canonically equivalent with different spellings: the classic
    /// NFC/NFD pair, macOS handing out NFD and Linux NFC.
    ///
    /// Not a warning either — it is the case the key exists for — but a
    /// destination that spells the name a different way does matter when
    /// writing: it is what [`SyncStep::dest_rel`] carries.
    Normalization,
    /// Joined by an ext4/f2fs `+F`'s FULL fold (0.45.0, #145): the
    /// expansion `straße.txt` and `strasse.txt` share at THAT location and
    /// nowhere else.
    ///
    /// It is different from [`PairTransform::CaseFold`] and not a nuance of
    /// it: two names that only pair by expanding **do not name the same
    /// text**. They are two texts a specific volume cannot sustain at once,
    /// which is exactly [`PairTransform::NormalizationSingleton`]'s
    /// situation — the other side of the comparison may have both files,
    /// and distinct. That is why [`PairTransform::names_one_text`] answers
    /// `false` here, and a sync plan does not overwrite over this pair.
    ///
    /// A 0.44 client reads it as [`PairTransform::Unknown`]
    /// (`#[serde(other)]`), which also answers `false`: it degrades to the
    /// prudent side without knowing why.
    FullFold,
    /// Paired by an NFC SINGLETON decomposition, and that is the one that
    /// **may be joining two distinct files**: U+212A KELVIN SIGN against
    /// `K`, U+2126 OHM SIGN against U+03A9. Unicode declares them
    /// canonically equivalent; ext4 stores them as two files and a reader
    /// sees them as two characters.
    ///
    /// Wins over the other two when they coincide: a pair that also case
    /// folds is still the dangerous one, and the consumer that only looks
    /// at this variant has to see it.
    ///
    /// **It is BEST-EFFORT and errs to the safe side.** Whoever produces it
    /// looks at whether either of the two names CONTAINS a character with a
    /// singleton decomposition, not whether that character is exactly the
    /// one that separates them: a pair that is also NFC/NFD and also
    /// carries an identical OHM SIGN on both sides gets marked here. The
    /// character set is tiny and none appears in an ordinary name, so the
    /// false positive costs one extra warning and the false negative would
    /// cost a file.
    NormalizationSingleton,
    /// A transform this decoder does not know (`#[serde(other)]`): an N+1
    /// daemon emitted it. **It cannot be read as "harmless"** — see
    /// [`PairTransform::names_one_text`].
    #[serde(other)]
    Unknown,
}

impl PairTransform {
    /// Do the two spellings safely name ONE SAME text?
    ///
    /// `true` solo para [`PairTransform::CaseFold`] y
    /// [`PairTransform::Normalization`], which are the two transforms whose
    /// pairing is the wanted behavior.
    /// [`PairTransform::NormalizationSingleton`] is `false` because it may
    /// join two distinct files, and [`PairTransform::Unknown`] too: a
    /// transform this binary cannot name also does not know if it is
    /// harmless, and the "I don't know" default has to be the prudent one.
    ///
    /// ```
    /// use norte_proto::methods::PairTransform;
    /// assert!(PairTransform::CaseFold.names_one_text());
    /// assert!(PairTransform::Normalization.names_one_text());
    /// assert!(!PairTransform::FullFold.names_one_text());
    /// assert!(!PairTransform::NormalizationSingleton.names_one_text());
    /// assert!(!PairTransform::Unknown.names_one_text());
    /// ```
    #[must_use]
    pub fn names_one_text(self) -> bool {
        matches!(self, Self::CaseFold | Self::Normalization)
    }
}

/// A side of the comparison (0.39.0, ADR 0048): the left pane is the one
/// that launched the comparison.
///
/// ```
/// use norte_proto::methods::Side;
/// assert_eq!(serde_json::to_string(&Side::Right).expect("json"), r#""right""#);
/// let future: Side = serde_json::from_str(r#""middle""#).expect("degrades");
/// assert_eq!(future, Side::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// The left side: the pane the comparison was launched from.
    Left,
    /// The right side.
    Right,
    /// A side this decoder does not know (`#[serde(other)]`).
    #[serde(other)]
    Unknown,
}

/// Which rungs of the cascade run (0.39.0, ADR 0048).
///
/// The default is what decides whether comparing two trees READS CONTENT:
/// `size` and `mtime` on, `hash` NOT. A user who did not ask to hash a
/// terabyte over SFTP must not end up doing it, so the expensive rung is
/// always explicit.
///
/// A PARTIAL object on the wire fills in from that same default instead of
/// failing: a peer that only wants to turn on hash sends `{"hash": true}`.
///
/// ```
/// use norte_proto::methods::CompareCriteria;
/// let d = CompareCriteria::default();
/// assert!(d.size && d.mtime && !d.hash, "the expensive rung is opt-in");
/// let partial: CompareCriteria = serde_json::from_str(r#"{"hash":true}"#).expect("partial");
/// assert_eq!(partial, CompareCriteria { size: true, mtime: true, hash: true });
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct CompareCriteria {
    /// Compare sizes. Default `true`.
    pub size: bool,
    /// Compare modification dates under tolerance. Default `true`.
    pub mtime: bool,
    /// Compare sha256 of the content of pairs the cheap rungs ruled equal.
    /// Default `false`: it READS both files whole, and in the daemon it
    /// also requires CONTENT scope.
    pub hash: bool,
}

impl Default for CompareCriteria {
    fn default() -> Self {
        Self {
            size: true,
            mtime: true,
            hash: false,
        }
    }
}

/// Default mtime tolerance: 2000 ms, the FAT rule — the widest real
/// granularity a filesystem this tree touches can have.
fn default_mtime_tolerance_ms() -> u32 {
    2000
}

/// ONE row of a comparison (0.39.0, ADR 0048): a pair, its verdict, and how
/// much that verdict is worth.
///
/// The row is emitted FINAL: none gets corrected afterward, so the wire
/// needs no row updates nor a panel to reconcile.
///
/// Carries both [`Entry`]s WHOLE and not two paths because the panel
/// paints size, date and each side's name BYTES, and the walk just listed
/// both directories: refetching per row would turn one listing into N
/// `fs.stat`s over two providers, with the tree already changing
/// underneath.
///
/// ```
/// use norte_proto::methods::{
///     CompareConfidence, CompareCriterion, CompareRow, CompareVerdict,
/// };
/// use norte_proto::{Entry, EntryKind, VPath};
/// let left_entry = Entry {
///     path: VPath::parse("file:///a/informe%FF%FE.dat").expect("path"),
///     kind: EntryKind::File,
///     size: Some(7),
///     mtime_ms: None,
///     attrs: Default::default(),
/// };
/// let row = CompareRow {
///     id: 1,
///     left: Some(left_entry),
///     right: None,
///     verdict: CompareVerdict::OnlyLeft,
///     criterion: CompareCriterion::Presence,
///     confidence: CompareConfidence::Certain,
///     newer: None,
///     reason: None,
///     side: None,
///     paired_under: None,
/// };
/// assert!(row.sides_are_consistent() && row.reason_is_consistent());
/// // What is absent does NOT travel: neither `null` nor the key (checked
/// // against the object's KEYS, not by substring: a path can contain
/// // "right").
/// let json = serde_json::to_string(&row).expect("json");
/// let obj: serde_json::Value = serde_json::from_str(&json).expect("object");
/// for absent in ["right", "newer", "reason", "side", "paired_under"] {
///     assert!(obj.get(absent).is_none(), "{absent} must not travel: {json}");
/// }
/// // And the non-UTF8 name comes back byte for byte (hard rule 1).
/// let back: CompareRow = serde_json::from_str(&json).expect("json");
/// assert_eq!(back, row);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareRow {
    /// Monotonic identifier within ONE comparison. The panel's selection
    /// anchors to it: a filter hides rows, it never renumbers them.
    pub id: u64,
    /// The left side's entry, if there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<Entry>,
    /// The right side's entry, if there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<Entry>,
    /// What the comparison concluded.
    pub verdict: CompareVerdict,
    /// Which rung decided it.
    pub criterion: CompareCriterion,
    /// How much that verdict is worth.
    pub confidence: CompareConfidence,
    /// Which side is NEWER, when the date decided the row. Nothing in this
    /// spec reads it: spec 2 (the sync plan) needs it to propose a
    /// direction, and producing it here costs nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newer: Option<Side>,
    /// The why, for the TWO verdicts that have a why
    /// ([`CompareVerdict::Ambiguous`] and [`CompareVerdict::Error`]). `None`
    /// for any other — see [`CompareRow::reason_is_consistent`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<CompareReason>,
    /// The side `reason` applies to, when it applies to just one: a read
    /// that failed only on the left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<Side>,
    /// Both sides paired and **their names are NOT the same bytes**: under
    /// what transform they paired (0.42.0, #152).
    ///
    /// `None` in the ordinary case — a pair of byte-for-byte identical
    /// names, or a row that does not have two sides — and that is why the
    /// key is OMITTED: an ordinary comparison's payload stays byte-for-byte
    /// 0.41.0's.
    ///
    /// It is independent of [`CompareRow::reason`] on purpose. A pairing by
    /// normalization does not make the row ambiguous — the verdict is a
    /// legitimate `Same` or `Different`, decided by whichever rung applied
    /// — and putting it in `reason` would have required relaxing
    /// [`CompareRow::reason_is_consistent`], which would make a 0.41 client
    /// see good rows fail its own consistency check.
    ///
    /// **What a client must NOT do is recompute it.** Both [`Entry`]s
    /// travel whole, so comparing the two names' bytes is possible; knowing
    /// whether case folding was in effect is not, because that comes from
    /// BOTH providers' [`Capabilities`](crate::Capabilities) and is a
    /// property of the pair, not of one side. Whoever pairs is the one who
    /// can answer, and this field is its answer.
    ///
    /// # Speaks of this row's NAME, not its whole path
    /// Pairing is per segment, and so is this field: it says how the two
    /// LAST segments paired, not whether some directory above paired by a
    /// transform. A `K/` (U+212A) directory against an ASCII `K/` comes out
    /// with its own row marked, it gets DESCENDED into — both are
    /// directories and they paired — and every child inside pairs by
    /// identical names and arrives with `None`.
    ///
    /// **A consumer deciding over a SUBTREE has to propagate the
    /// ancestor's mark itself.** Rows arrive in pre-order — the directory
    /// before its content — so it can be done; what cannot be done is read
    /// row by row and believe a `None` means "this path is safe". Protected
    /// file by file, a `Mirror` would keep mirroring an entire subtree
    /// under a directory that only pairs by a singleton
    /// (`protocol-guardian`, W4b, MAJOR-2).
    ///
    /// # Invariants (the core maintains them; a client may assume them)
    /// `Some` ⟹ the row has BOTH sides: a transform is a property of a
    /// pair, and a row with only one side does not have one. In particular
    /// a [`CompareVerdict::Ambiguous`] — which belongs to ONE side by
    /// definition — never carries it. A [`CompareVerdict::Error`] CAN, if
    /// it brings both entries: the directory paired and what failed was
    /// listing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paired_under: Option<PairTransform>,
}

impl CompareRow {
    /// Do the verdict and the present sides agree?
    ///
    /// The invariant the wire cannot express: [`CompareVerdict::OnlyLeft`]
    /// implies `right: None`, and `Same`/`Different`/`TypeMismatch` imply
    /// both sides. It is deliberately NOT a `Deserialize` rejection: a
    /// malformed row has to degrade like a bad attribute cell, not kill the
    /// whole batch. The daemon asserts it in its tests; a client uses it to
    /// decide whether to trust the row.
    ///
    /// [`CompareVerdict::Ambiguous`], [`CompareVerdict::Error`] and
    /// [`CompareVerdict::Unknown`] have no rule to break: the first names a
    /// ONE-sided collision, the second may have no entry to show, and about
    /// the third — a verdict from an N+1 daemon — this client knows
    /// nothing. Inventing a rule for them would make an N-1 client distrust
    /// legitimate rows.
    ///
    /// ```
    /// # use norte_proto::methods::{CompareConfidence, CompareCriterion, CompareRow, CompareVerdict};
    /// # use norte_proto::{Entry, EntryKind, VPath};
    /// # fn entry() -> Entry {
    /// #     Entry { path: VPath::parse("file:///a").expect("path"), kind: EntryKind::File,
    /// #             size: None, mtime_ms: None, attrs: Default::default() }
    /// # }
    /// # fn row(verdict: CompareVerdict, left: Option<Entry>, right: Option<Entry>) -> CompareRow {
    /// #     CompareRow { id: 1, left, right, verdict, criterion: CompareCriterion::Presence,
    /// #                  confidence: CompareConfidence::Certain, newer: None, reason: None, side: None,
    /// #                  paired_under: None }
    /// # }
    /// assert!(row(CompareVerdict::OnlyLeft, Some(entry()), None).sides_are_consistent());
    /// assert!(!row(CompareVerdict::OnlyLeft, Some(entry()), Some(entry())).sides_are_consistent());
    /// assert!(!row(CompareVerdict::Same, Some(entry()), None).sides_are_consistent());
    /// ```
    #[must_use]
    pub fn sides_are_consistent(&self) -> bool {
        let (l, r) = (self.left.is_some(), self.right.is_some());
        match self.verdict {
            CompareVerdict::OnlyLeft => l && !r,
            CompareVerdict::OnlyRight => r && !l,
            CompareVerdict::Same | CompareVerdict::Different | CompareVerdict::TypeMismatch => {
                l && r
            }
            CompareVerdict::Ambiguous | CompareVerdict::Error | CompareVerdict::Unknown => true,
        }
    }

    /// Do the verdict and the reason agree?
    ///
    /// `reason` is `Some` for EXACTLY two verdicts —
    /// [`CompareVerdict::Ambiguous`] and [`CompareVerdict::Error`] — and
    /// `None` for the rest: anywhere else it would be noise a client would
    /// have to guess at. [`CompareVerdict::Unknown`] is EXEMPT for the same
    /// reason as in [`CompareRow::sides_are_consistent`]: a verdict this
    /// client does not know can legitimately carry a reason.
    ///
    /// ```
    /// # use norte_proto::methods::{CompareConfidence, CompareCriterion, CompareReason, CompareRow, CompareVerdict};
    /// # fn row(verdict: CompareVerdict, reason: Option<CompareReason>) -> CompareRow {
    /// #     CompareRow { id: 1, left: None, right: None, verdict, criterion: CompareCriterion::Presence,
    /// #                  confidence: CompareConfidence::Unknown, newer: None, reason, side: None,
    /// #                  paired_under: None }
    /// # }
    /// assert!(row(CompareVerdict::Ambiguous, Some(CompareReason::CaseFold)).reason_is_consistent());
    /// assert!(!row(CompareVerdict::Ambiguous, None).reason_is_consistent());
    /// assert!(!row(CompareVerdict::Same, Some(CompareReason::CaseFold)).reason_is_consistent());
    /// ```
    #[must_use]
    pub fn reason_is_consistent(&self) -> bool {
        match self.verdict {
            CompareVerdict::Ambiguous | CompareVerdict::Error => self.reason.is_some(),
            CompareVerdict::Unknown => true,
            _ => self.reason.is_none(),
        }
    }
}

/// Params of [`CONNECTION_CLOSE`] (0.49.0, #140).
///
/// ```
/// use norte_proto::methods::ConnectionCloseParams;
/// let p: ConnectionCloseParams =
///     serde_json::from_str(r#"{"path":"sftp://host/casa"}"#).expect("params");
/// assert_eq!(p.path.scheme(), "sftp");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionCloseParams {
    /// ANY path of the connection. It closes by `scheme://authority`,
    /// which is how the core has it cached: the frontend sends the place
    /// where the pane is and does not need to know how a session is keyed
    /// internally.
    pub path: VPath,
}

/// Result of [`CONNECTION_CLOSE`].
///
/// ```
/// use norte_proto::methods::ConnectionCloseResult;
/// let r = ConnectionCloseResult { closed: true };
/// assert!(serde_json::to_value(&r).expect("json")["closed"].as_bool().expect("bool"));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionCloseResult {
    /// `true` if there was a session and it got released; `false` if there
    /// was none.
    ///
    /// Not an error: whoever disconnects wants to end up without a
    /// connection, and they already are. It says so so a frontend can
    /// distinguish "I closed it" from "there was nothing", which is the
    /// difference between a useful message and one that lies.
    pub closed: bool,
}

/// Params of [`FS_DIR_SIZE`] (0.49.0, #139).
///
/// ```
/// use norte_proto::methods::FsDirSizeParams;
/// let p: FsDirSizeParams =
///     serde_json::from_str(r#"{"paths":["file:///a"]}"#).expect("params");
/// assert_eq!(p.paths.len(), 1);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FsDirSizeParams {
    /// What has to be measured. SEVERAL roots on purpose: what the human
    /// has marked is a selection, and summing it at once gives ONE number
    /// — the one that answers "does this fit at the destination?" — instead
    /// of N tasks they would have to add up by hand.
    ///
    /// A loose file is valid: it counts its own size and walks nothing.
    /// Empty is `-32602`: measuring nothing is not a request.
    pub paths: Vec<VPath>,
}

/// An archive format that CAN be written (0.50.0, #132).
///
/// Fewer than the ones that can be read, on purpose: `rar` is delegated to
/// an external program and read-only (ADR 0056), and 7z is not even read.
/// An enum and not a free string: the set is closed and the server does
/// not have to validate vocabulary.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormat {
    /// zip with `deflate`, or `store` at level 0.
    #[default]
    Zip,
    /// Plain tar, uncompressed.
    Tar,
    /// tar compressed with gzip.
    TarGz,
}

/// Params of [`ARCHIVE_PACK`] (0.50.0, #132).
///
/// ```
/// use norte_proto::methods::{ArchiveFormat, ArchivePackParams};
/// let p: ArchivePackParams = serde_json::from_str(
///     r#"{"sources":["file:///a/x"],"dest":"file:///a.zip","base":"file:///a","format":"zip"}"#,
/// )
/// .expect("params");
/// assert_eq!(p.format, ArchiveFormat::Zip);
/// assert_eq!(p.level, None, "the level IS chosen by the core");
/// // And without `format` it does NOT parse: it is the client's decision, not a default.
/// assert!(
///     serde_json::from_str::<ArchivePackParams>(
///         r#"{"sources":["file:///a/x"],"dest":"file:///a.zip","base":"file:///a"}"#,
///     )
///     .is_err()
/// );
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivePackParams {
    /// What gets packed. A directory enters with its tree.
    pub sources: Vec<VPath>,
    /// The archive being created. It must NOT exist: overwriting here
    /// would be a silent loss, and the frontend already knows how to ask.
    pub dest: VPath,
    /// Format, explicit and MANDATORY. See [`ARCHIVE_PACK`] for why it is
    /// not inferred from the name on the server — and why it has no
    /// default either: with one, `{"dest":"backup.tar.gz"}` without
    /// `format` would produce a ZIP named `backup.tar.gz`, silently and
    /// contradicting the name. That is worse than the inference this
    /// method rejects. Requiring it on a NEW type costs no compatibility;
    /// requiring it later would break it.
    pub format: ArchiveFormat,
    /// Compression level 0..=9, or `None` for the core's. 0 is "store
    /// without compressing" in the formats that allow it.
    ///
    /// A value over 9 is TRIMMED to 9 instead of rejected: the level is a
    /// preference, not a request, and throwing out a half-hour packing job
    /// over a 42 would be worse than compressing it well.
    #[serde(default)]
    pub level: Option<u8>,
    /// The directory the STORED names are computed against.
    ///
    /// Without this, "pack these three marks" has no defined name for each
    /// entry, and two clients choosing differently would give two
    /// different archives from the same request. Every `source` has to
    /// fall under this base.
    pub base: VPath,
}

/// Params of [`ARCHIVE_TEST_REPORT`] (0.50.0, #132).
///
/// ```
/// use norte_proto::methods::ArchiveTestReportParams;
/// let p: ArchiveTestReportParams =
///     serde_json::from_str(r#"{"task_id":7}"#).expect("params");
/// assert_eq!(p.task_id.get(), 7);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveTestReportParams {
    /// The Task whose report is requested.
    pub task_id: crate::TaskId,
}

/// Params of [`ARCHIVE_TEST`] (0.50.0, #132).
///
/// ```
/// use norte_proto::methods::ArchiveTestParams;
/// let p: ArchiveTestParams =
///     serde_json::from_str(r#"{"path":"file:///a.zip"}"#).expect("params");
/// assert_eq!(p.path.to_wire(), "file:///a.zip");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveTestParams {
    /// The container, as a file (not as an internal root): what gets
    /// tested is the whole archive, not one of its entries.
    pub path: VPath,
}

/// An entry that did not pass [`ARCHIVE_TEST`].
///
/// ```
/// use norte_proto::methods::ArchiveTestFailure;
/// let f: ArchiveTestFailure =
///     serde_json::from_str(r#"{"name":"a.txt","reason":"crc"}"#).expect("failure");
/// assert_eq!(f.reason, "crc");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArchiveTestFailure {
    /// The entry that failed, WHOLE and in its wire form — the only one
    /// that preserves the bytes (rule 1).
    ///
    /// It started out as just the lossy `name` underneath, and both halves
    /// of that were wrong: `a/x.txt` and `b/x.txt` reported the same
    /// thing, and a non-UTF8 name came back as `U+FFFD` with nothing
    /// saying which of the two it was. This report is the ONLY place the
    /// corrupt entry gets named, so it has to be able to point at it — the
    /// same criterion as [`RenameStuckStep`], which carries `VPath` for the
    /// same reason.
    #[serde(default = "wire_empty")]
    pub path: String,
    /// The name to SHOW, with the lossy conversion any other painted name
    /// carries. It accompanies [`Self::path`]; it does not replace it.
    pub name: String,
    /// Failure category, OPEN vocabulary comparable by equality: `crc`,
    /// `truncated`, `unsupported`, `io`. It can GROW additively — a future
    /// format fails in ways this set does not have — so a client receiving
    /// one it does not know shows it as-is and never rejects the report
    /// over it. Same contract as [`ConnectionDegraded::reason`].
    pub reason: String,
}

/// The default `path` of an [`ArchiveTestFailure`] deserialized without it
/// (a report from a 0.50 daemon before the field existed).
fn wire_empty() -> String {
    String::new()
}

/// Result of [`ARCHIVE_TEST`] (0.50.0, #132).
///
/// ```
/// use norte_proto::methods::ArchiveTestResult;
/// let r: ArchiveTestResult = serde_json::from_str(r#"{"entries":3}"#).expect("result");
/// assert!(r.failed.is_empty() && !r.truncated);
/// assert!(r.checked.is_empty(), "without saying what got checked, nothing is asserted");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArchiveTestResult {
    /// Entries walked.
    pub entries: u64,
    /// The ones that failed, up to [`ARCHIVE_TEST_MAX_FAILURES`].
    pub failed: Vec<ArchiveTestFailure>,
    /// `true` if there were more failures than fit in `failed`.
    pub truncated: bool,
    /// WHAT actually got checked: `crc` when the format carries a
    /// per-entry sum, `gzip_crc` for a `tar.gz`'s tail, `sizes` when the
    /// only verifiable thing is that each declared size is reachable.
    ///
    /// `snake_case`, like every other token this protocol coins (`tar_gz`,
    /// `dir_size`, `rename_batch`): a hyphen here was an invitation for a
    /// client to write `gzip_crc`, find nothing, and never light up the
    /// `tar.gz` case. All three travel in the golden.
    ///
    /// It goes in the result and not in documentation because "passes"
    /// means different things per format, and a client that paints
    /// "intact" over a plain tar would be asserting what the format cannot
    /// support.
    pub checked: Vec<String>,
}

/// Params of [`ARCHIVE_PACK_REPORT`] (0.58.0, #250).
///
/// ```
/// use norte_proto::methods::ArchivePackReportParams;
/// let p: ArchivePackReportParams =
///     serde_json::from_str(r#"{"task_id":7}"#).expect("params");
/// assert_eq!(p.task_id.get(), 7);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivePackReportParams {
    /// The packing whose report is requested (the `task_id`
    /// [`ARCHIVE_PACK`] returned).
    pub task_id: TaskId,
}

/// An entry whose name MEANS something else on another system (0.58.0, #250).
///
/// ```
/// use norte_proto::methods::PackRiskyName;
/// let r: PackRiskyName =
///     serde_json::from_str(r#"{"path":"a%5Cb.txt","name":"a\\b.txt","risk":"separator"}"#)
///         .expect("risk");
/// assert_eq!(r.risk, "separator");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PackRiskyName {
    /// The name STORED inside the archive: relative to the packing's base,
    /// separated by `/`, and **percent-encoded over the raw bytes** — the
    /// only thing that preserves a non-UTF8 name (rule 1).
    ///
    /// **This is NOT a [`VPath`] and must not be parsed as one**, and that
    /// is where it splits from [`ArchiveTestFailure::path`], which does
    /// carry a full, parseable wire path. That one names an entry INSIDE a
    /// container that exists somewhere; this one names a relative entry
    /// that only means something inside the archive that was just written.
    ///
    /// The codec is not the same either: here everything that is not
    /// `[A-Za-z0-9._~-]` gets escaped, so `café.txt` travels as
    /// `caf%C3%A9.txt` and not literal. `/` is left UNESCAPED on purpose:
    /// it is the archive's component separator, a Unix name component
    /// cannot carry a raw `/`, and everything else goes escaped — so it
    /// stays unambiguous, and hiding it would make unreadable exactly the
    /// name one has to go look for.
    pub path: String,
    /// The same one, to SHOW, with the lossy conversion any other painted
    /// name carries. It accompanies [`Self::path`]; it does not replace it
    /// — over a non-UTF8 name, this one carries `U+FFFD` and that one is
    /// the only one the bytes can be recovered from.
    pub name: String,
    /// What is wrong with it outside: `separator` (`\` is a directory
    /// separator in 7-Zip and Explorer), `stream` (`:` opens an alternate
    /// stream on NTFS), `reserved` (`CON`, `NUL`, `AUX`… cannot be
    /// extracted on Windows at all), `trailing` (a trailing dot or space
    /// Windows eats without saying so).
    ///
    /// OPEN vocabulary comparable by equality, with the SAME contract as
    /// [`ArchiveTestFailure::reason`] and [`ConnectionDegraded::reason`]:
    /// it can GROW additively — another platform deforms names in ways this
    /// set does not have — so **a client receiving a class it does not
    /// know shows it as-is and NEVER rejects the report over it**. Treating
    /// these four as exhaustive is misreading the contract.
    pub risk: String,
}

/// Result of [`ARCHIVE_PACK_REPORT`] (0.58.0, #250): what that archive
/// carries inside that means something else outside here.
///
/// **Empty means it was checked and there was none**, not that it was not
/// checked — but within what [`Self::checked`] declares and not one
/// millimeter beyond. This report does NOT assert the archive travels
/// intact anywhere: it asserts that the classes it says it looked at did
/// not appear. `<`, `>`, `"`, `|`, `?` and `*` are also illegal on Windows
/// and are not looked at today, and without `checked` a clean report would
/// be saying the opposite. It is the same reason
/// [`ArchiveTestResult::checked`] exists: "passes" means different things
/// depending on what was checked.
///
/// What does NOT appear here are folding collisions — two entries that on
/// macOS or NTFS would be a single file — and not by oversight: **those do
/// not get packed**. `archive.pack` fails with
/// [`ConflictKind::Exists`](crate::ConflictKind) before writing a byte,
/// because there a file IS actually LOST on extraction. A list that can
/// never carry anything would be worse than not having one.
///
/// ```
/// use norte_proto::methods::ArchivePackReportResult;
/// let r: ArchivePackReportResult =
///     serde_json::from_str(r#"{"entries":12,"checked":["separator"]}"#).expect("report");
/// assert!(r.risky.is_empty() && !r.truncated);
/// assert_eq!(r.entries, 12, "how many entries were checked");
/// // And without `checked`, a clean report authorizes saying nothing.
/// let mute: ArchivePackReportResult =
///     serde_json::from_str(r#"{"entries":12}"#).expect("report");
/// assert!(mute.checked.is_empty());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArchivePackReportResult {
    /// Entries checked. With `risky` empty, this is what turns the report
    /// into an assertion instead of a silence.
    pub entries: u64,
    /// WHAT risk classes actually got looked at: `separator`, `stream`,
    /// `reserved`, `trailing`. OPEN and GROWING vocabulary, same as
    /// [`PackRiskyName::risk`] and for the same reason as
    /// [`ArchiveTestResult::checked`]: a clean report only means something
    /// alongside the list of what was checked, and a client painting
    /// "travels intact" over a daemon that looks at four classes would be
    /// asserting what nobody checked.
    ///
    /// Empty = **nothing is declared**, and then an empty `risky` authorizes
    /// saying nothing. Same treatment as `checked` in its twin.
    pub checked: Vec<String>,
    /// Entries whose name means something else outside, up to
    /// [`ARCHIVE_PACK_REPORT_MAX`].
    pub risky: Vec<PackRiskyName>,
    /// `true` if the list ran short. A trimmed report that did not say so
    /// would show "three names" over an archive with four hundred.
    pub truncated: bool,
}

/// Limit of [`ArchivePackReportResult`]'s list elements (0.58.0).
///
/// The same criterion as [`ARCHIVE_TEST_MAX_FAILURES`]: a report is meant
/// to be READ, and a list of thousands is not read — what one does with it
/// is scroll until giving up. What a limit cannot do is lie about what it
/// left out, and that is what [`ArchivePackReportResult::truncated`] is
/// for.
pub const ARCHIVE_PACK_REPORT_MAX: usize = 64;

/// Params of [`FILE_SPLIT`] (0.50.0, #132).
///
/// ```
/// use norte_proto::methods::FileSplitParams;
/// let p: FileSplitParams = serde_json::from_str(
///     r#"{"path":"file:///g.iso","part_bytes":1048576,"dest_dir":"file:///trozos"}"#,
/// )
/// .expect("params");
/// assert_eq!(p.part_bytes, 1_048_576);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSplitParams {
    /// The file being split. It is not touched: the chunks are new files.
    pub path: VPath,
    /// Bytes per chunk, at least [`FILE_SPLIT_MIN_BYTES`]. The last one can
    /// be smaller; if the split is exact there is NO empty chunk at the
    /// end.
    pub part_bytes: u64,
    /// Where the chunks are left.
    pub dest_dir: VPath,
}

/// Params of [`FILE_COMBINE`] (0.50.0, #132).
///
/// ```
/// use norte_proto::methods::FileCombineParams;
/// let p: FileCombineParams =
///     serde_json::from_str(r#"{"first":"file:///g.iso.001","dest":"file:///g.iso"}"#)
///         .expect("params");
/// assert_eq!(p.first.to_wire(), "file:///g.iso.001");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileCombineParams {
    /// The FIRST chunk (`.001`). The rest is found by convention, and a
    /// gap is an error instead of a join across it.
    pub first: VPath,
    /// The file being created. It must not exist.
    pub dest: VPath,
}

/// Params of [`FS_COMPARE`] (0.39.0, ADR 0048).
///
/// ```
/// use norte_proto::methods::{CompareCriteria, FsCompareParams};
/// // The MINIMUM that must be sent: two roots. Everything else has a default.
/// let p: FsCompareParams =
///     serde_json::from_str(r#"{"left":"file:///a","right":"file:///b"}"#).expect("params");
/// assert_eq!(p.criteria, CompareCriteria::default());
/// assert_eq!(p.mtime_tolerance_ms, 2000);
/// assert!(p.max_depth.is_none() && !p.follow_symlinks && p.descend_orphans.is_none());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsCompareParams {
    /// Left root: the pane that launched the comparison.
    pub left: VPath,
    /// Right root. If it resolves to the same provider and path as `left`,
    /// the request is `-32602` and no Task gets created.
    pub right: VPath,
    /// Which rungs run. Absent = [`CompareCriteria::default`].
    #[serde(default)]
    pub criteria: CompareCriteria,
    /// Maximum descent depth, counting the root as 0. `None` = no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// The mtime rung's tolerance in milliseconds. Default 2000 (the FAT
    /// rule). It is a REQUEST parameter and not a provider capability:
    /// touching `Capabilities`'s wire for a single consumer was not worth
    /// it.
    ///
    /// `u32` and not `i64` (MAJOR finding from protocol-guardian, C1
    /// review) even though [`Entry::mtime_ms`](crate::Entry::mtime_ms) is
    /// `i64`: a NEGATIVE tolerance makes `|Δ| > tolerance` true for every
    /// pair, so one extra sign in a client's request would turn two
    /// identical trees into an entire tree of `Different` — a quiet, wrong
    /// answer on the hot path — instead of an error. With `u32` the
    /// DESERIALIZER rejects it, which is stronger than any check the
    /// handler could forget, and the ceiling (49 days) is plenty for any
    /// filesystem's granularity.
    #[serde(default = "default_mtime_tolerance_ms")]
    pub mtime_tolerance_ms: u32,
    /// Follow symlinks. Default `false`, and today it is the only thing
    /// the core implements: targets are compared AS BYTES
    /// ([`CompareCriterion::LinkTarget`]), which makes cycle detection
    /// unnecessary.
    #[serde(default)]
    pub follow_symlinks: bool,
    /// Descend into directories that exist ONLY on this side (0.40.0).
    /// Absent — the default, and everything 0.39.0 knew how to do — emits
    /// ONE row for the orphan and does not walk it.
    ///
    /// One side, never both: the type enforces it, and the reason is in
    /// [`SyncCompareOptions::descend_orphans`], the same field seen from a
    /// plan. Here, on the other hand, **it IS the caller's call**: "show
    /// me everything that is only on the left, not just the tip" is a
    /// legitimate comparison request.
    ///
    /// It is a [`DescendSide`] and not a [`Side`]: a misspelled side dies
    /// in any peer's DESERIALIZER (`-32602`) instead of degrading to
    /// [`Side::Unknown`] — which is no side at all — and silently serving a
    /// different set of rows than requested. The long reason is in
    /// [`DescendSide`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descend_orphans: Option<DescendSide>,
}

/// A BATCH of [`COMPARE_ROWS`] rows (0.39.0, ADR 0048). Bounded by
/// [`COMPARE_ROWS_MAX_BATCH`] and coalesced server-side, the same contract
/// as [`SearchHits`].
///
/// ```
/// use norte_proto::TaskId;
/// use norte_proto::methods::CompareRowsBatch;
/// let b = CompareRowsBatch { task_id: TaskId::new(7), rows: vec![] };
/// assert_eq!(
///     serde_json::to_string(&b).expect("json"),
///     r#"{"task_id":7,"rows":[]}"#
/// );
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareRowsBatch {
    /// Owning Task (correlates with `fs.compare` → `task_id`).
    pub task_id: TaskId,
    /// The rows of this batch, in the order the walk produced them. Never
    /// more than [`COMPARE_ROWS_MAX_BATCH`].
    pub rows: Vec<CompareRow>,
}

/// A path RELATIVE to a plan's two roots (0.40.0, ADR 0049): zero or more
/// [`Segment`]s, in BYTES (hard rule 1). The empty sequence is the ROOT.
///
/// Wire: percent-encoded segments (ADR 0001, [`VPath`]'s same codec)
/// joined by `/`, so the root is the empty string. A `rel` naming three
/// levels costs one string, not an object — and half a million steps pass
/// through it.
///
/// # Why not a [`VPath`]
/// A [`VPath`] is "always absolute with respect to the provider's root"
/// and ALWAYS carries a scheme and an authority. Fitting a relative path
/// inside forces inventing them, and what gets invented is visible and
/// harmful:
///
/// - A plan from `file:///…` to `sftp://nas/…` would send `file:///sub` as
///   the relative path against an sftp destination. A third party reading
///   the schema will send `sftp://nas/sub`, equally "correct", and the two
///   do not compare equal.
/// - `plan_hash` covers the plan's conclusions, and `rel` is one of them: a
///   scheme the daemon is ordered to IGNORE cannot be inside the token used
///   to approve it. A field cannot be ignored and hashed.
/// - "A `rel` never escapes its root" becomes a property of the TYPE:
///   [`Segment::new`] already rejects `/`, `.`, `..` and NUL, and validates
///   it POST-decode, so a `%2E%2E` smuggles no `..`. Any peer's
///   deserializer checks it, not a check that can be forgotten.
///
/// ```
/// use norte_proto::methods::RelPath;
/// let r = RelPath::parse_wire("sub/informe%FF%FE.dat").expect("rel");
/// assert_eq!(r.segments().len(), 2);
/// assert_eq!(r.segments()[1].as_bytes(), b"informe\xff\xfe.dat");
/// assert_eq!(r.to_wire(), "sub/informe%FF%FE.dat");
/// // The empty string is the ROOT, and it travels as such.
/// assert!(RelPath::default().is_root());
/// assert_eq!(serde_json::to_string(&RelPath::default()).expect("json"), r#""""#);
/// // What would escape the root never gets to exist.
/// assert!(RelPath::parse_wire("../etc").is_err());
/// assert!(RelPath::parse_wire("%2E%2E/etc").is_err());
/// assert!(RelPath::parse_wire("/a").is_err());
/// assert!(RelPath::parse_wire("a//b").is_err());
/// // Not even over the wire, which is where it matters.
/// assert!(serde_json::from_str::<RelPath>(r#""a/../b""#).is_err());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(into = "String")]
pub struct RelPath(Vec<Segment>);

impl RelPath {
    /// Builds from already-validated segments.
    #[must_use]
    pub fn new(segments: Vec<Segment>) -> Self {
        Self(segments)
    }

    /// Parses the wire form: percent-encoded segments joined by `/`. The
    /// EMPTY string is the root.
    ///
    /// # Errors
    /// Whatever [`Segment::parse_wire`] returns for any of the segments:
    /// [`VPathError::EmptySegment`] (an extra `/`, at the start, at the
    /// end, or doubled), [`VPathError::DotSegment`] (`.`/`..`, checked
    /// after decoding), [`VPathError::NulByte`] or
    /// [`VPathError::BadEscape`].
    pub fn parse_wire(wire: &str) -> Result<Self, VPathError> {
        if wire.is_empty() {
            return Ok(Self::default());
        }
        wire.split('/')
            .map(Segment::parse_wire)
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }

    /// The canonical wire form.
    #[must_use]
    pub fn to_wire(&self) -> String {
        let mut out = String::with_capacity(self.0.len() * 12);
        for (i, seg) in self.0.iter().enumerate() {
            if i > 0 {
                out.push('/');
            }
            out.push_str(&seg.to_wire());
        }
        out
    }

    /// The segments, in order.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.0
    }

    /// `true` if it names the root itself (no segments).
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// `path`'s path RELATIVE to `root`, or `None` if it does not hang off
    /// it.
    ///
    /// It lives here because the three types it touches live here —
    /// [`VPath`], [`Segment`] and this one — and because the answer has to
    /// be ONE: the transducer that produces the steps and the frontend
    /// assembling the request DERIVE their `rel`s through here, which is
    /// what makes the wire-string comparison the core's `include` filter
    /// does afterward mean something. Two implementations that diverged
    /// would send an `include` that does not select what the reader
    /// marked.
    ///
    /// # How it compares, and why like this
    /// Scheme, authority and then the segments ONE BY ONE by their raw
    /// bytes: no `to_str`, no lossy, no normalizing and no case folding
    /// (hard rule 1). Being by SEGMENTS and not by string prefix is what
    /// stops `…/ab` from hanging off `…/a`.
    ///
    /// The authority is also compared BYTE FOR BYTE, and that is
    /// deliberate even though a DNS hostname is not case-sensitive: for
    /// `mem://` and for an object-storage connection id, the authority is
    /// an opaque token, and folding it would join two distinct
    /// connections. It fails CLOSED — a `None`, never one write too many.
    ///
    /// # What the caller has to decide
    /// The result cannot escape the root (a [`Segment`] cannot be `..`),
    /// but it CAN be the root itself (`path == root`), which in a sync
    /// plan is the most destructive target there is and in a
    /// [`SyncBlocker`] is the CORRECT value — a read-only destination hangs
    /// off the root. Which of the two it is, is something the caller
    /// knows, so it is returned and decided there.
    ///
    /// And the same with `None`: **it fails closed as long as the caller
    /// turns it into a REFUSAL**. Both of today's do (the plan dies, the
    /// selection is rejected). A caller that read it as "skip this row"
    /// would turn a strict comparison into a silent filter, which is the
    /// only way this has to be dangerous.
    ///
    /// ```
    /// use norte_proto::VPath;
    /// use norte_proto::methods::RelPath;
    /// let root = VPath::parse("file:///origen").expect("root");
    /// let path = VPath::parse("file:///origen/sub/a.txt").expect("path");
    /// assert_eq!(
    ///     RelPath::under(&root, &path).expect("hangs off it").to_wire(),
    ///     "sub/a.txt"
    /// );
    /// // By SEGMENTS, not by string prefix.
    /// let otro = VPath::parse("file:///origenes/a.txt").expect("path");
    /// assert!(RelPath::under(&root, &otro).is_none());
    /// // The root itself comes out as the ROOT, and deciding what to do
    /// // about that is the caller's.
    /// assert!(RelPath::under(&root, &root).expect("is the root").is_root());
    /// ```
    #[must_use]
    pub fn under(root: &VPath, path: &VPath) -> Option<Self> {
        if path.scheme() != root.scheme() || path.authority() != root.authority() {
            return None;
        }
        let mut rest = path.segments();
        for root_segment in root.segments() {
            if rest.next() != Some(root_segment) {
                return None;
            }
        }
        // Unreachable: each of these bytes came out of a `Segment` a
        // `VPath` already validated. Mapped to `None` instead of `expect`
        // (rule 6).
        let rest = rest.map(Segment::new).collect::<Result<Vec<_>, _>>().ok()?;
        Some(Self::new(rest))
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_wire())
    }
}

impl From<RelPath> for String {
    fn from(value: RelPath) -> Self {
        value.to_wire()
    }
}

impl<'de> Deserialize<'de> for RelPath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = String::deserialize(deserializer)?;
        Self::parse_wire(&wire).map_err(serde::de::Error::custom)
    }
}

// The serde is by hand (validates on deserialize), so the schema is too.
#[cfg(feature = "schema")]
impl schemars::JsonSchema for RelPath {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "RelPath".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A path RELATIVE to a sync plan's two roots: \
                            percent-encoded segments joined by `/`, with the \
                            empty string meaning the root itself. It carries no \
                            scheme and no authority — the same `rel` names an \
                            entry under both roots, which may be different \
                            providers. `.`, `..`, an empty segment and a NUL are \
                            rejected when decoding (after percent-decoding, so \
                            `%2E%2E` smuggles nothing), which is what makes \
                            \"a rel never escapes its root\" a property of the \
                            wire rather than a check a server can forget.",
        })
    }
}

/// In which direction a plan synchronizes (0.40.0, ADR 0049).
///
/// # Why this enum has NO `#[serde(other)]`
/// It travels CLIENT→DAEMON. A token this daemon does not know dies in the
/// deserializer and the request is `-32602`, unlike all of [`CompareRow`]'s
/// vocabulary — which travels daemon→client and degrades. Accepting an
/// unknown mode "by default" is accepting DELETION by default, and the
/// default would have to be one of the two: there is no neutral value. The
/// same holds for [`OnUnknown`].
///
/// That [`OnUnknown`] does have a default and this one does not is not an
/// inconsistency: they are two different wire facts. An ABSENT KEY says "I
/// have no opinion" and deserves a documented default; an UNKNOWN TOKEN
/// says "I have an opinion this daemon cannot honor" and dies the same way
/// in both enums. And where `on_unknown`'s default is safe to assume is
/// here: `mode` decides whether the plan EVER GETS deletion steps at all,
/// while `on_unknown` only splits rows between two step classes the mode
/// already authorized — and the human sees the result, with its
/// `confidence` and its reversal, before approving. A wrong `on_unknown` is
/// SEEN before acting; a wrong `mode` would produce a different plan.
///
/// `#[non_exhaustive]` yes, for #126's reason: a future mode cannot break
/// anyone's `match` outside this crate.
///
/// ```
/// use norte_proto::methods::SyncMode;
/// assert_eq!(serde_json::to_string(&SyncMode::Mirror).expect("json"), r#""mirror""#);
/// // A mode from the future does NOT degrade: it gets rejected.
/// assert!(serde_json::from_str::<SyncMode>(r#""obliterate""#).is_err());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    /// Copy to the destination what is missing and what differs. NEVER
    /// deletes: what the destination has extra stays where it is.
    Update,
    /// [`SyncMode::Update`] plus deleting from the destination what the
    /// source does not have. It is the only mode that emits
    /// [`SyncStepKind::DeleteTree`].
    Mirror,
}

/// The side whose orphans are ENUMERATED (0.40.0, ADR 0049): the value of
/// [`FsCompareParams::descend_orphans`] and of
/// [`SyncCompareOptions::descend_orphans`].
///
/// # Why it is not a [`Side`]
///
/// Because this field travels CLIENT→DAEMON and [`Side`] does not: [`Side`]
/// names the side of a row or a blocker the daemon EMITS, so it carries
/// `#[serde(other)]` and a `"lft"` turns into [`Side::Unknown`] instead of
/// dying. As a request parameter that would be a silently overwritten
/// value of the worst class: `Unknown` is no side, so the comparison would
/// not descend into ANY of them and the caller would receive — without a
/// single error — a different set of rows than requested, over a
/// three-letter typo.
///
/// It is the same rule [`SyncMode`] and [`OnUnknown`] already follow, and
/// the reason the check does not live in the handler: an `if` in
/// `handle_fs_compare` does not reach the EMBEDDED arm
/// (`CoreBackend::Embedded` calls the engine without going through the
/// daemon), and what the type forbids has no handler that can forget it.
///
/// `#[non_exhaustive]` for #126's reason, like the other two.
///
/// ```
/// use norte_proto::methods::{DescendSide, Side};
/// assert_eq!(serde_json::to_string(&DescendSide::Left).expect("json"), r#""left""#);
/// // A typo does NOT degrade: it dies in the deserializer.
/// assert!(serde_json::from_str::<DescendSide>(r#""lft""#).is_err());
/// // And "unknown", which `Side` does accept, is also not a side that can be requested.
/// assert!(serde_json::from_str::<DescendSide>(r#""unknown""#).is_err());
/// assert_eq!(Side::from(DescendSide::Right), Side::Right);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum DescendSide {
    /// The left side (`left` from [`FsCompareParams`], `source` from
    /// [`SyncPlanParams`] when the source is the left one).
    Left,
    /// The right side.
    Right,
}

impl From<DescendSide> for Side {
    /// The requested side, already as the [`Side`] the engine speaks. In
    /// this direction the conversion is total; the reverse does not exist
    /// on purpose, because [`Side::Unknown`] has no destination here.
    fn from(side: DescendSide) -> Self {
        match side {
            DescendSide::Left => Self::Left,
            DescendSide::Right => Self::Right,
        }
    }
}

/// What to do with a row whose confidence is
/// [`CompareConfidence::Unknown`] (0.40.0, ADR 0049): the provider could
/// not say whether both sides are equal.
///
/// The default is [`OnUnknown::Copy`]: faced with "cannot be known",
/// copying costs bandwidth and skipping silently costs stale data.
/// Whichever choice, the step keeps `confidence: unknown`, so the report
/// can explain why it wrote.
///
/// Client→daemon: WITHOUT `#[serde(other)]`, for the reason written in
/// [`SyncMode`].
///
/// ```
/// use norte_proto::methods::OnUnknown;
/// assert_eq!(OnUnknown::default(), OnUnknown::Copy);
/// assert!(serde_json::from_str::<OnUnknown>(r#""maybe""#).is_err());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum OnUnknown {
    /// Copy anyway (default).
    #[default]
    Copy,
    /// Do not touch it, and say so: [`SyncStepKind::Skip`] with
    /// [`SyncReason::UnknownConfidence`].
    Skip,
}

/// What ONE step of a plan does (0.40.0, ADR 0049).
///
/// Daemon→client: `#[serde(other)]`, like ADR 0048's four enums. An N+1
/// daemon adding a step class cannot kill an N-1 client's batch of
/// [`SYNC_STEPS_MAX_BATCH`] steps.
///
/// ```
/// use norte_proto::methods::SyncStepKind;
/// assert_eq!(
///     serde_json::to_string(&SyncStepKind::DeleteTree).expect("json"),
///     r#""delete_tree""#
/// );
/// let future: SyncStepKind = serde_json::from_str(r#""teleport""#).expect("degrades");
/// assert_eq!(future, SyncStepKind::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum SyncStepKind {
    /// Create in the destination a directory that only exists in the
    /// source.
    CreateDir,
    /// Copy to the destination an entry that is missing.
    Copy,
    /// Write over a destination entry that differs. With a trash at the
    /// destination it is "to the trash and copy"; without one it is
    /// [`StepReversal::Irreversible`].
    Overwrite,
    /// Delete from the destination a tree the source does not have. ONLY
    /// under [`SyncMode::Mirror`], and it is ONE step for the whole tree: one
    /// move to the trash, one journal entry, one thing to restore.
    DeleteTree,
    /// Touch nothing, and say why ([`SyncStep::reason`] populated). It is
    /// emitted only for what is NOTABLE — a source collision, a confidence
    /// the user asked to skip, an unreadable entry —: two identical trees
    /// produce ZERO steps, not a million `Skip`s.
    Skip,
    /// A class this decoder does not know (`#[serde(other)]`). The core
    /// never emits it.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

/// How an already-executed step reverts (0.40.0, ADR 0049).
///
/// It is a function of the step's class and of whether the DESTINATION has
/// a trash, and it travels in the plan — before approval — precisely so the
/// human sees how many steps cannot be undone BEFORE saying yes, and not
/// in the report afterward.
///
/// # This column ALONE does not say whether a step reverts, and whoever
/// paints a dialog has to read it alongside [`SyncPlanDone::dest_trash`]
///
/// It says how the step would revert *where the destination can return
/// it*, which is not the same question. A [`StepReversal::Delete`] against
/// a destination without a trash is emitted the same and the undo SKIPS it
/// (see that variant), so a copy plan painted from this column alone
/// promises an undo that will not happen. The complete answer is the pair
/// `(reversal, dest_trash)`, and it is written in [`DestTrash`].
///
/// Daemon→client: `#[serde(other)]`.
///
/// ```
/// use norte_proto::methods::StepReversal;
/// assert_eq!(
///     serde_json::to_string(&StepReversal::RestoreTrash).expect("json"),
///     r#""restore_trash""#
/// );
/// let future: StepReversal = serde_json::from_str(r#""time_travel""#).expect("degrades");
/// assert_eq!(future, StepReversal::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum StepReversal {
    /// Undo = delete what this step created. Nothing was destroyed, so the
    /// step IS reversible wherever the destination has a trash.
    ///
    /// **With [`DestTrash::Absent`] the undo does NOT execute it**, and
    /// this variant still travels. It is not an oversight: deleting
    /// "whatever is at that path today" with no trash to come back from can
    /// destroy work the human did AFTER syncing (#65), so the undo skips it
    /// and counts it in `skipped_created_no_trash`. Marking the step
    /// `Irreversible` would lie in the other direction — over a
    /// destination with a trash it comes back whole — so the truth does
    /// not fit in this column: it has to be read alongside
    /// [`SyncPlanDone::dest_trash`].
    Delete,
    /// Undo = take out of the trash what this step buried (and, on an
    /// `Overwrite`, first delete what it wrote: the journal walks `seq`
    /// descending, so the order works itself out).
    RestoreTrash,
    /// Cannot be undone, and the plan says so BEFOREHAND (hard rule 4:
    /// either there is undo or there is an explicit classification with its
    /// reason, which goes in [`SyncStep::reason`]).
    Irreversible,
    /// A reversal this decoder does not know (`#[serde(other)]`). The core
    /// never emits it.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

/// Why a step is a [`SyncStepKind::Skip`] or is
/// [`StepReversal::Irreversible`] (0.40.0, ADR 0049). CLOSED vocabulary: the
/// core never invents a reason.
///
/// Daemon→client: `#[serde(other)]`.
///
/// ```
/// use norte_proto::methods::SyncReason;
/// assert_eq!(
///     serde_json::to_string(&SyncReason::NoTrashOnTarget).expect("json"),
///     r#""no_trash_on_target""#
/// );
/// let future: SyncReason = serde_json::from_str(r#""solar_flare""#).expect("degrades");
/// assert_eq!(future, SyncReason::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum SyncReason {
    /// Two SOURCE names collapse onto the same pairing key
    /// ([`CompareVerdict::Ambiguous`] on that side): it is not known which
    /// of the two to copy, so neither gets copied and the rest of the plan
    /// stands. On the DESTINATION the same collision is not a reason but a
    /// blocker ([`SyncBlockerKind::AmbiguousDest`]): only one of the two
    /// sides can lose data.
    AmbiguousSource,
    /// [`OnUnknown::Skip`] and the criterion earned
    /// [`CompareConfidence::Unknown`].
    UnknownConfidence,
    /// Could not READ what was needed, on the side that mattered
    /// ([`CompareVerdict::Error`]).
    Unreadable,
    /// **The destination has no trash to COME BACK from**, so this step
    /// cannot be undone. ALWAYS accompanies [`StepReversal::Irreversible`].
    ///
    /// Covers two cases, and whoever paints it must not promise they are
    /// the same:
    ///
    /// - the destination has no trash, and what this step buries is
    ///   nowhere;
    /// - the destination DOES have a trash but does not say where it
    ///   leaves things (`Provider::trash_restorable` at `false`: macOS and
    ///   Windows). What got buried gets pulled out by hand from the
    ///   system's trash, but norte's undo cannot guess which one it was —
    ///   and then NO step of the plan is reversible, not even a copy,
    ///   because undoing a creation also goes through the trash (#65).
    ///
    /// The wire token does not distinguish the two on purpose: they are
    /// the same consequence for whoever approves, and separating them
    /// would be a new variant (a protocol bump) for one sentence.
    ///
    /// The distinction does travel, but of the PLAN and not of the step:
    /// [`SyncPlanDone::dest_trash`] separates "there is no trash"
    /// ([`DestTrash::Absent`]) from "there is one and it does not say
    /// where it leaves things" ([`DestTrash::Opaque`]), which is where it
    /// makes sense — it is a property of the destination, the same for
    /// every step — and where a dialog can read it once.
    NoTrashOnTarget,
    /// **Both sides paired by a transform that may join DISTINCT files**,
    /// so the plan does not act on that pair (0.43.0, #207).
    ///
    /// The motivating case is
    /// [`PairTransform::NormalizationSingleton`]: `K.txt` with U+212A KELVIN
    /// SIGN against `K.txt` with the ASCII `K`. Unicode declares them
    /// canonically equivalent, ext4 stores them as two files, and an
    /// `Overwrite` over that pair writes one's bytes over the other's —
    /// which is the data loss #152 described.
    ///
    /// The criterion is [`PairTransform::names_one_text`] and not the
    /// concrete variant: it skips EVERY transform this binary cannot
    /// assert names a single text, including one from a newer daemon. The
    /// ordinary ones — [`PairTransform::CaseFold`] and
    /// [`PairTransform::Normalization`] — keep acting: they are the pairs
    /// the pairing key exists for, and denying them would break the
    /// macOS↔Linux case it serves.
    ///
    /// It is a `Skip` and NOT a blocker on purpose: the plan remains
    /// approvable and the rest of the tree syncs. A blocker would leave the
    /// whole tree unsynced over one odd pair, and the dangerous row is
    /// visible in the plan the same way before approving anything.
    ///
    /// An N-1 client decodes it as [`SyncReason::Unknown`] and paints "a
    /// reason this version cannot name": it acts neither more nor less,
    /// because the step is already a `Skip` on the wire.
    NonInjectivePairing,
    /// A reason this decoder does not know (`#[serde(other)]`). The core
    /// never emits it.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

/// Why a plan CANNOT be executed (0.40.0, ADR 0049). CLOSED vocabulary.
///
/// A blocker is not a failed step: it is a reason the WHOLE plan cannot be
/// approved ([`SyncPlanDone::executable`] at `false`). Daemon→client:
/// `#[serde(other)]`.
///
/// ```
/// use norte_proto::methods::SyncBlockerKind;
/// assert_eq!(
///     serde_json::to_string(&SyncBlockerKind::OverlapDetected).expect("json"),
///     r#""overlap_detected""#
/// );
/// let future: SyncBlockerKind = serde_json::from_str(r#""cosmic_ray""#).expect("degrades");
/// assert_eq!(future, SyncBlockerKind::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum SyncBlockerKind {
    /// Two DESTINATION names collapse onto the same pairing key: writing
    /// there is writing over one of two files without knowing which.
    AmbiguousDest,
    /// The walk reached the OTHER root: both name the same tree. The
    /// earlier structural check
    /// ([`Error::OverlappingRoots`](crate::Error::OverlappingRoots)) can be
    /// defeated by a symlink, an SFTP root under two authorities, or an
    /// archive opened through two paths; this one cannot.
    OverlapDetected,
    /// The destination's provider does not accept writes (see
    /// `Capabilities`). It is raised BEFORE planning a single step: writes
    /// are not planned against a tree that refuses them.
    DestReadOnly,
    /// A DESTINATION directory over [`COMPARE_MAX_DIR_ENTRIES`]. In a
    /// comparison that costs a row and the walk continues; in a plan that
    /// is going to write there, no: what is in that directory is not
    /// known.
    DirTooLarge,
    /// A [`CompareVerdict::TypeMismatch`] where one of the two sides is a
    /// DIRECTORY: swapping a tree for a file (or vice versa) is a
    /// destructive structural change, and this spec does not promise it.
    ///
    /// The rest of the class mismatches remain a
    /// [`SyncStepKind::Overwrite`]: replacing a symlink with a file — or a
    /// device, a socket or any `EntryKind::Other` — is replacing bytes,
    /// and that is exactly what the step means. A directory is not: an
    /// `Overwrite` normatively says "to the trash and copy bytes", the
    /// step carries no [`EntryKind`] to distinguish it with, and the
    /// subtree involved is not even in the plan — the walk does not
    /// descend into a pair that is not two directories. So a human
    /// decides.
    ///
    /// [`SyncBlocker::side`] names the side that has the DIRECTORY (plan
    /// convention: [`Side::Left`] the source, [`Side::Right`] the
    /// destination) and is ALWAYS present in this class, because it is
    /// what distinguishes "delete a destination tree to put a file" from
    /// "do not copy a source tree over a file" — see
    /// [`SyncBlocker::shape_is_consistent`]. Only one of the two sides can
    /// be it: if both were directories there would be no mismatch.
    ///
    /// # Why the SOURCE side also blocks
    /// It is the exception to the rule this family follows twice — a
    /// source collision is a [`SyncReason::AmbiguousSource`] and a
    /// destination one a [`SyncBlockerKind::AmbiguousDest`]; a source
    /// directory that is too large is a `Skip` and a destination one a
    /// [`SyncBlockerKind::DirTooLarge`] — and the exception is deliberate.
    ///
    /// Skipping the entry, which is what a `Skip` would do, loses nothing
    /// IMMEDIATE: the source tree stays there and the destination file
    /// too. What it loses is the mode's promise. Whoever asked for
    /// [`SyncMode::Mirror`] asked for the destination to end up like the
    /// source, and with a file where a tree should be it does not: the
    /// plan would say yes and the result would say no, and that divergence
    /// is STRUCTURAL — an entire subtree that will never arrive — and not
    /// a loose entry a report can list. A name collision is different:
    /// there it is not known WHAT to copy, and not copying is the only
    /// safe answer.
    ///
    /// The price is measured and accepted: a single one of these mismatches
    /// in a tree of a hundred thousand files leaves the whole plan
    /// unapproved, and the remedy is fixing that name or bounding the plan
    /// with `include`. If someday `Skip` is preferred, a new [`SyncReason`]
    /// is needed — CLOSED daemon→client vocabulary, meaning a bump and a
    /// compatibility argument.
    TypeMismatchDir,
    /// A name the DESTINATION cannot have (0.52.0, #163).
    ///
    /// Decided by the destination's provider (`Provider::name_is_legal`),
    /// which is the one that knows its rules: `CON`, `f:ads`, a trailing
    /// dot or space — all legal on ext4 — are not on NTFS, and `f:ads` is
    /// the worst of the four because there it **works**: it writes an
    /// alternate stream, and the copy says it went fine while the file is
    /// not there.
    ///
    /// Blocks instead of skipping for the same reason as
    /// [`Self::TypeMismatchDir`]: whoever asked for a mirror asked for the
    /// destination to end up like the source, and a name that cannot exist
    /// there is a structural divergence no later report fixes. The remedy
    /// is renaming at the source or bounding the plan with `include`.
    IllegalDestName,
    /// A class this decoder does not know (`#[serde(other)]`). The core
    /// never emits it.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

/// ONE step of a sync plan (0.40.0, ADR 0049): what is going to be done, to
/// what, why, and how it reverts.
///
/// `criterion` and `confidence` travel PER STEP and not per plan: it is the
/// obligation ADR 0048 left pending, settled. A report can say "I
/// overwrote it because the date could not be read" instead of "I
/// overwrote it", and whoever reviews a sync that went wrong sees which
/// rung authorized each write.
///
/// ```
/// use norte_proto::methods::{
///     CompareConfidence, CompareCriterion, RelPath, StepReversal, SyncStep, SyncStepKind,
/// };
/// let s = SyncStep {
///     id: 1,
///     kind: SyncStepKind::Copy,
///     rel: RelPath::parse_wire("sub/informe%FF%FE.dat").expect("rel"),
///     dest_rel: None,
///     size: Some(1234),
///     criterion: CompareCriterion::Presence,
///     confidence: CompareConfidence::Certain,
///     reversal: Some(StepReversal::Delete),
///     reason: None,
/// };
/// assert!(s.shape_is_consistent());
/// // What is absent does NOT travel: neither `null` nor the key.
/// let json = serde_json::to_value(&s).expect("json");
/// assert!(json.get("reason").is_none() && json.get("dest_rel").is_none());
/// // And the non-UTF8 name comes back byte for byte (hard rule 1).
/// let back: SyncStep = serde_json::from_value(json).expect("json");
/// assert_eq!(back, s);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStep {
    /// Monotonic identifier within ONE plan. The panel's cursor anchors to
    /// it: a filter hides steps, it never renumbers them. Does NOT enter
    /// `plan_hash` — it is presentation, not a conclusion.
    pub id: u64,
    /// What the step does.
    pub kind: SyncStepKind,
    /// The path, RELATIVE to the plan's roots, in BYTES (hard rule 1).
    /// Neither a `String` nor a [`VPath`]: the two roots may be different
    /// providers, so it has no scheme to carry — see [`RelPath`].
    ///
    /// # With respect to WHICH of the two roots (normative)
    /// The side the step is about, which is almost always the SOURCE:
    /// `dest_root + rel` names the same entry, except when
    /// [`SyncStep::dest_rel`] says otherwise — and that field exists
    /// precisely because "almost always" is not "always".
    ///
    /// The exceptions are steps that only speak about the DESTINATION,
    /// where `rel` is relative to `dest_root` and there is no source path
    /// to name: a [`SyncStepKind::DeleteTree`], and the
    /// [`SyncStepKind::Skip`] of a destination listing that did not let
    /// itself be read. The step carries no field distinguishing this —
    /// adding a side for two forms that do not write was not worth it —
    /// so a panel anchoring every `rel` to the source pane will paint
    /// those two in the wrong place.
    pub rel: RelPath,
    /// What the entry is called ON THE DESTINATION, relative to the
    /// destination root, when its bytes are NOT `rel`'s.
    ///
    /// # The rule, normative
    /// The DESTINATION path this step lands on, present if and ONLY if its
    /// bytes differ from `rel`'s (hard rule 1: bytes are compared, never
    /// strings, and never after folding). `None` — the common case, and
    /// why the key travels ABSENT — means "on the destination it is named
    /// exactly `rel`".
    ///
    /// Whoever executes the step reads `source_root + rel` and writes
    /// `dest_root + dest_rel.unwrap_or(rel)`.
    ///
    /// Present does NOT assert something exists there, and whoever reads
    /// it must not infer that: today the core only populates it from a
    /// destination entry the row carried, but the rule is about the PATH,
    /// not what is in it. It also does not assert what kind of thing is
    /// there: a byte-identical path may today be a symlink pointing
    /// outside the tree, and only the executor opening it can resolve
    /// that.
    ///
    /// # Why it is needed
    /// The comparison pairs by a FOLDED key — always NFC, case-folded when
    /// either side does not distinguish case — so a legitimate pair can
    /// have two names of different bytes: an NFC `café` from the source
    /// against the NFD `café` from the destination, a `README` against an
    /// APFS's `readme`. Without this field a [`SyncStepKind::Overwrite`]
    /// would be written under the SOURCE's name, which on ext4 creates a
    /// SECOND file next to the one meant to be overwritten; and its
    /// [`StepReversal::RestoreTrash`] would promise to pull out of the
    /// trash something nobody buried. See
    /// <https://github.com/compilando/norte/issues/152>.
    ///
    /// # Why the destination does NOT get renamed
    /// Spelling the destination the way the source spells it would turn
    /// every macOS↔Linux sync into a renaming dance — NFD and NFC are THE
    /// SAME name to whoever reads it — and that churn is exactly what this
    /// tree exists to not produce. It writes where it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_rel: Option<RelPath>,
    /// Bytes this step MOVES, when known; a provider that gives no size
    /// leaves `None`.
    ///
    /// A [`SyncStepKind::DeleteTree`], a [`SyncStepKind::CreateDir`] and a
    /// [`SyncStepKind::Skip`] move none and leave it ABSENT.
    ///
    /// The rule is NORMATIVE even though almost nothing enforces it:
    /// [`SyncCounts::add`] IGNORES the size of a step that moves no bytes —
    /// so an extra size does not corrupt the number the human approves —
    /// and [`SyncStep::shape_is_consistent`] does not look at it either.
    /// What does notice it is `plan_hash`, which feeds the field no matter
    /// what: two plans that only differ in a size placed where it should
    /// not be are two different plans, and need two approvals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Which cascade rung decided the row this step comes from.
    pub criterion: CompareCriterion,
    /// How much that decision is worth.
    pub confidence: CompareConfidence,
    /// How it undoes. `None` if and ONLY if `kind` is
    /// [`SyncStepKind::Skip`] — see [`SyncStep::shape_is_consistent`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reversal: Option<StepReversal>,
    /// The why, for the two forms that have a why: a `Skip` and an
    /// [`StepReversal::Irreversible`] step. `None` for any other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<SyncReason>,
}

impl SyncStep {
    /// Do `kind`, `reversal`, `reason` and `dest_rel` agree with each
    /// other?
    ///
    /// THREE of the invariants the wire cannot express, stated ONCE, here:
    /// `reversal` is `None` if and only if `kind` is
    /// [`SyncStepKind::Skip`], `reason` is `Some` for EXACTLY a `Skip` and
    /// a step whose reversal is [`StepReversal::Irreversible`], and
    /// [`SyncStep::dest_rel`] — which exists to name the OTHER spelling —
    /// cannot be the same as `rel`: there the key is redundant, and a
    /// consumer seeing it repeated is reading a step its producer did not
    /// compute properly.
    ///
    /// The other two from the design do not fit in a single step and are
    /// not checked here: "`DeleteTree` only under `Mirror`" needs the mode,
    /// which does not travel in the step, and "`blockers` non-empty ⟹
    /// `!executable`" belongs to [`SyncPlanDone`]. `size` is not looked at
    /// either: its rule — a `Skip` and a `DeleteTree` leave it absent —
    /// belongs to the COUNTERS, and a step that broke it is still an
    /// executable step.
    ///
    /// It is deliberately NOT a `Deserialize` rejection, for the same
    /// reason as [`CompareRow::reason_is_consistent`]: a malformed step has
    /// to degrade like a bad attribute cell, not kill a batch of
    /// [`SYNC_STEPS_MAX_BATCH`]. The daemon asserts it in its tests; a
    /// client uses it to decide whether to trust the step.
    ///
    /// [`SyncStepKind::Unknown`] and [`StepReversal::Unknown`] are EXEMPT: a
    /// step from a daemon one version ahead is not something this client
    /// can judge, and asserting otherwise would make it distrust legitimate
    /// steps. The `dest_rel` rule still reaches a step whose REVERSAL is
    /// unknown — it does not depend on it — but not one whose CLASS is
    /// unknown: there the exemption is total, and it can afford to be
    /// because a repeated `dest_rel` is redundant and not dangerous (both
    /// branches of `dest_rel.unwrap_or(rel)` give the same path).
    ///
    /// ```
    /// # use norte_proto::methods::{
    /// #     CompareConfidence, CompareCriterion, RelPath, StepReversal, SyncReason, SyncStep,
    /// #     SyncStepKind,
    /// # };
    /// # fn step(kind: SyncStepKind, reversal: Option<StepReversal>, reason: Option<SyncReason>) -> SyncStep {
    /// #     SyncStep { id: 1, kind, rel: RelPath::parse_wire("a").expect("rel"), dest_rel: None,
    /// #                size: None, criterion: CompareCriterion::Presence,
    /// #                confidence: CompareConfidence::Certain, reversal, reason }
    /// # }
    /// assert!(step(SyncStepKind::Copy, Some(StepReversal::Delete), None).shape_is_consistent());
    /// assert!(!step(SyncStepKind::Copy, None, None).shape_is_consistent());
    /// assert!(
    ///     step(SyncStepKind::Skip, None, Some(SyncReason::Unreadable)).shape_is_consistent()
    /// );
    /// assert!(!step(SyncStepKind::Skip, None, None).shape_is_consistent());
    /// // `dest_rel` names the OTHER spelling, so repeating `rel` is noise.
    /// let mut s = step(SyncStepKind::Overwrite, Some(StepReversal::Delete), None);
    /// s.dest_rel = Some(s.rel.clone());
    /// assert!(!s.shape_is_consistent());
    /// s.dest_rel = Some(RelPath::parse_wire("A").expect("rel"));
    /// assert!(s.shape_is_consistent());
    /// ```
    #[must_use]
    pub fn shape_is_consistent(&self) -> bool {
        if self.kind == SyncStepKind::Unknown {
            return true;
        }
        // Compared by BYTES — [`Segment`]'s `Eq` does it — the same
        // comparison whoever produces the step used to decide whether to
        // populate it: folding here would validate exactly the pair this
        // field exists to distinguish.
        if self.dest_rel.as_ref() == Some(&self.rel) {
            return false;
        }
        let reversal_ok = if self.kind == SyncStepKind::Skip {
            self.reversal.is_none()
        } else {
            self.reversal.is_some()
        };
        if self.reversal == Some(StepReversal::Unknown) {
            return reversal_ok;
        }
        let owes_reason =
            self.kind == SyncStepKind::Skip || self.reversal == Some(StepReversal::Irreversible);
        reversal_ok && self.reason.is_some() == owes_reason
    }
}

/// A BATCH of [`SYNC_STEPS`] steps (0.40.0, ADR 0049). Bounded by
/// [`SYNC_STEPS_MAX_BATCH`] and coalesced server-side, the same contract as
/// [`CompareRowsBatch`].
///
/// ```
/// use norte_proto::TaskId;
/// use norte_proto::methods::SyncStepsBatch;
/// let b = SyncStepsBatch { task_id: TaskId::new(7), steps: vec![] };
/// assert_eq!(
///     serde_json::to_string(&b).expect("json"),
///     r#"{"task_id":7,"steps":[]}"#
/// );
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStepsBatch {
    /// Owning Task (correlates with `sync.plan` → `task_id`).
    pub task_id: TaskId,
    /// The steps of this batch, in execution ORDER (the walk is pre-order,
    /// so a `CreateDir` precedes every copy inside it with no need to
    /// sort). Never more than [`SYNC_STEPS_MAX_BATCH`].
    pub steps: Vec<SyncStep>,
}

/// What a plan adds up to (0.40.0, ADR 0049). The approval dialog opens
/// with `irreversible`.
///
/// It is filled step by step with [`SyncCounts::add`], which is where the
/// rules of what counts where are written — once.
///
/// ```
/// use norte_proto::methods::SyncCounts;
/// let c = SyncCounts { copy: 2, bytes: 30, ..SyncCounts::default() };
/// assert_eq!(serde_json::to_value(&c).expect("json")["delete_tree"], 0);
/// // The byte total ALWAYS comes accompanied by how many steps do not know it.
/// assert_eq!(serde_json::to_value(&c).expect("json")["unmeasured_steps"], 0);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncCounts {
    /// Directories to create.
    pub create_dir: u64,
    /// Entries to copy.
    pub copy: u64,
    /// Entries to overwrite.
    pub overwrite: u64,
    /// Trees to delete from the destination (only under
    /// [`SyncMode::Mirror`]).
    pub delete_tree: u64,
    /// Steps that touch nothing and say why.
    pub skip: u64,
    /// Steps of a class this decoder does NOT know
    /// ([`SyncStepKind::Unknown`]): a daemon one version ahead emitted
    /// something this client cannot classify.
    ///
    /// The core ALWAYS leaves it at zero — it never emits a step it cannot
    /// name — so it only fills in an N-1 client that sums the
    /// [`SYNC_STEPS`] batches on its own. It exists for the same reason as
    /// `unmeasured_steps`: without it, those steps would not appear in ANY
    /// counter and the sum of the five classes would say the plan is
    /// smaller than it is, which is approving a chunk of the plan blindly.
    pub unknown_kind: u64,
    /// Steps whose reversal is [`StepReversal::Irreversible`]. Counted
    /// SEPARATELY because it is the one number a human must not have to
    /// derive.
    ///
    /// It CUTS ACROSS classes — an `Overwrite`, a `DeleteTree` and an
    /// unknown-class step that declares itself irreversible all add up
    /// here — so it does not sum with the class counters: it is read
    /// alongside them.
    pub irreversible: u64,
    /// Bytes the plan moves, from the steps that MOVE bytes and carry a
    /// size. A deletion and a `Skip` move none.
    ///
    /// ALWAYS read alongside `unmeasured_steps`: on its own it is a lower
    /// bound, not a total — [`SyncCounts::exact_bytes`] is the total or
    /// nothing. The sum saturates at `u64::MAX`, so a value exactly equal
    /// to `u64::MAX` may be a ceiling and not a measurement.
    ///
    /// It is NOT the same as [`SyncReportResult::bytes`], which are the
    /// bytes ACTUALLY moved when executing: over `file://` the two numbers
    /// differ as a matter of course, so this one does not serve as a
    /// progress bar's denominator.
    pub bytes: u64,
    /// How many steps move bytes WITHOUT knowing how many
    /// ([`SyncStep::size`] absent).
    ///
    /// It is not a rare case: an orphan does not get hydrated
    /// (<https://github.com/compilando/norte/issues/157>) and
    /// `norte-vfs-local` lists with `size: None`, so over `file://` it is
    /// the NORMAL case. Without this field, `bytes` would read zero and
    /// the approval dialog would confidently say a 40 GB plan moves
    /// nothing.
    ///
    /// Counting them separately is the rule ADR 0048 already set for this
    /// family: "the provider cannot say" is an answer, not an error, and
    /// it is not hidden inside a number that looks certain. The dialog
    /// reads "1.2 GB + 340 files of unknown size". Hydrating the sizes is
    /// a later optimization (issue #156) that can only SHRINK this number,
    /// never change its shape.
    ///
    /// They are STEPS, not bytes — the name says so, and that is why it
    /// says so: next to `bytes`, a `bytes_unknown` would read as "7 bytes
    /// we don't know" instead of "7 steps we could not measure". Only
    /// [`SyncStepKind::Copy`] and [`SyncStepKind::Overwrite`] increment it,
    /// so `unmeasured_steps <= copy + overwrite` always, and a consumer can
    /// verify that before trusting counters it did not compute itself.
    pub unmeasured_steps: u64,
}

impl SyncCounts {
    /// Sums ONE step. The rules of what counts where, written once.
    ///
    /// - Every class sums into its own counter, [`SyncStepKind::Unknown`]
    ///   included (`unknown_kind`): putting it into another counter would
    ///   lie about what the plan does, but not counting it in any would
    ///   lie about HOW MUCH plan there is. The `match` is EXHAUSTIVE on
    ///   purpose — `#[non_exhaustive]` does not apply inside the crate
    ///   that defines the enum — so a new class breaks compilation here
    ///   instead of silently stopping being counted.
    /// - `irreversible` sums for ANY class whose reversal is
    ///   [`StepReversal::Irreversible`], the unknown one included: it is
    ///   the number a human must not have to derive, and not knowing what
    ///   class of step it is does not make it less irreversible.
    /// - Only [`SyncStepKind::Copy`] and [`SyncStepKind::Overwrite`] move
    ///   bytes. A `CreateDir` writes no content, and a `DeleteTree` and a
    ///   `Skip` write nothing — a size on any of them is IGNORED instead
    ///   of summed, because a counted byte that never moves is the
    ///   approval dialog lying.
    /// - Of the ones that do move, the one that carries a size sums into
    ///   `bytes` and the one that does not sums ONE into
    ///   `unmeasured_steps`. Never a faked zero.
    ///
    /// The sums are saturating: an overflowed counter is a weird number,
    /// but a panic on the path of a half-million-step plan is a dead Task.
    ///
    /// ```
    /// use norte_proto::methods::{
    ///     CompareConfidence, CompareCriterion, RelPath, StepReversal, SyncCounts, SyncStep,
    ///     SyncStepKind,
    /// };
    /// let mk_step = |kind, size| SyncStep {
    ///     id: 1,
    ///     kind,
    ///     rel: RelPath::parse_wire("a").expect("rel"),
    ///     dest_rel: None,
    ///     size,
    ///     criterion: CompareCriterion::Presence,
    ///     confidence: CompareConfidence::Certain,
    ///     reversal: Some(StepReversal::Delete),
    ///     reason: None,
    /// };
    /// let mut c = SyncCounts::default();
    /// c.add(&mk_step(SyncStepKind::Copy, Some(10)));
    /// c.add(&mk_step(SyncStepKind::Copy, None));
    /// assert_eq!((c.copy, c.bytes, c.unmeasured_steps), (2, 10, 1));
    /// // And with an unmeasured step, the EXACT total does not exist.
    /// assert_eq!(c.exact_bytes(), None);
    /// ```
    pub fn add(&mut self, step: &SyncStep) {
        // EXHAUSTIVE, no wildcard: `#[non_exhaustive]` does not apply
        // inside the crate that defines the enum, so a new class breaks
        // compilation here instead of silently stopping being counted.
        // Bytes are decided in the SAME `match` for the same reason:
        // whoever adds a class that writes content has to say at the same
        // time whether it sums bytes.
        match step.kind {
            SyncStepKind::CreateDir => self.create_dir = self.create_dir.saturating_add(1),
            SyncStepKind::Copy => {
                self.copy = self.copy.saturating_add(1);
                self.add_bytes(step.size);
            }
            SyncStepKind::Overwrite => {
                self.overwrite = self.overwrite.saturating_add(1);
                self.add_bytes(step.size);
            }
            SyncStepKind::DeleteTree => self.delete_tree = self.delete_tree.saturating_add(1),
            SyncStepKind::Skip => self.skip = self.skip.saturating_add(1),
            SyncStepKind::Unknown => self.unknown_kind = self.unknown_kind.saturating_add(1),
        }
        if step.reversal == Some(StepReversal::Irreversible) {
            self.irreversible = self.irreversible.saturating_add(1);
        }
    }

    /// The bytes of a step that DOES move bytes: summed if known, counted
    /// separately if not.
    fn add_bytes(&mut self, size: Option<u64>) {
        match size {
            Some(bytes) => self.bytes = self.bytes.saturating_add(bytes),
            None => self.unmeasured_steps = self.unmeasured_steps.saturating_add(1),
        }
    }

    /// The EXACT byte total, or `None` if some step could not be measured.
    ///
    /// It is the `Option<u64>` that `bytes` deliberately is NOT. Both
    /// fields travel over the wire because a lower bound plus the size of
    /// the ignorance ("1.2 GB + 340 unmeasured files") is a sentence that
    /// can be shown, and a `None` is not — over `file://` it would also be
    /// the normal case, so the dialog would never have anything to say.
    /// Whoever truly needs the total or nothing asks for it here and does
    /// not re-derive it.
    ///
    /// ```
    /// use norte_proto::methods::SyncCounts;
    /// let exact = SyncCounts { copy: 1, bytes: 10, ..SyncCounts::default() };
    /// assert_eq!(exact.exact_bytes(), Some(10));
    /// let partial = SyncCounts { unmeasured_steps: 1, ..exact };
    /// assert_eq!(partial.exact_bytes(), None, "an unmeasured step is not zero");
    /// ```
    #[must_use]
    pub fn exact_bytes(&self) -> Option<u64> {
        (self.unmeasured_steps == 0).then_some(self.bytes)
    }
}

/// Why a plan cannot be executed, with its location (0.40.0, ADR 0049).
///
/// ```
/// use norte_proto::methods::{RelPath, Side, SyncBlocker, SyncBlockerKind};
/// let b = SyncBlocker {
///     rel: RelPath::parse_wire("LEEME").expect("rel"),
///     kind: SyncBlockerKind::AmbiguousDest,
///     side: Some(Side::Right),
/// };
/// assert_eq!(serde_json::to_value(&b).expect("json")["kind"], "ambiguous_dest");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncBlocker {
    /// Where, RELATIVE to the two roots and in BYTES, same as
    /// [`SyncStep::rel`]. A blocker that is not about a specific location
    /// (a read-only destination) carries it empty: the root
    /// ([`RelPath::is_root`]).
    pub rel: RelPath,
    /// What class of blocker.
    pub kind: SyncBlockerKind,
    /// The side it happened on, when it happened on just one.
    ///
    /// # The convention, normative
    /// In a plan the SOURCE is [`Side::Left`] and the DESTINATION
    /// [`Side::Right`], **always**, and it has nothing to do with which
    /// pane launched the comparison: a sync request names `source` and
    /// `dest` ([`SyncPlanParams`]) and carries no [`Side`] at all, so
    /// within this family there is no second coordinate system to confuse
    /// it with. A frontend that synced right to left paints a `right` in
    /// its LEFT pane.
    ///
    /// Absent when the blocker is not about one side: an overlap is about
    /// both roots at once.
    ///
    /// **ALWAYS present for [`SyncBlockerKind::TypeMismatchDir`]**, the
    /// only one whose side cannot be deduced from the class — see
    /// [`SyncBlocker::shape_is_consistent`].
    ///
    /// (`Error::OverlappingRoots` does NOT use this convention and it
    /// should not be looked for there: it carries a
    /// [`RootOverlap`](crate::RootOverlap) precisely because "they are the
    /// same tree" is a third case two sides cannot say.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<Side>,
}

impl SyncBlocker {
    /// Do `kind` and `side` agree?
    ///
    /// The invariant the wire cannot express, stated ONCE, here:
    /// [`SyncBlockerKind::TypeMismatchDir`] ALWAYS carries `side`. It is
    /// the only blocker whose side cannot be deduced from its class —
    /// `AmbiguousDest`, `DirTooLarge` and `DestReadOnly` belong to the
    /// destination by definition, and an overlap belongs to neither — and
    /// at the same time the only one where the side IS the sentence: "I
    /// don't copy a source tree over a file" and "I don't delete a
    /// destination tree to put a file" are two different things, and
    /// without `side` there is none to paint.
    ///
    /// It is NOT a `Deserialize` rejection, for the same reason as
    /// [`SyncStep::shape_is_consistent`]: a malformed blocker has to
    /// degrade, not kill the whole list. And
    /// [`SyncBlockerKind::Unknown`] is EXEMPT: a blocker from a daemon one
    /// version ahead is not something this client can judge.
    ///
    /// ```
    /// use norte_proto::methods::{RelPath, Side, SyncBlocker, SyncBlockerKind};
    /// let mut b = SyncBlocker {
    ///     rel: RelPath::parse_wire("build").expect("rel"),
    ///     kind: SyncBlockerKind::TypeMismatchDir,
    ///     side: Some(Side::Right),
    /// };
    /// assert!(b.shape_is_consistent());
    /// b.side = None;
    /// assert!(!b.shape_is_consistent(), "with no side there is no sentence to paint");
    /// // An overlap is not about one side, and that is correct.
    /// b.kind = SyncBlockerKind::OverlapDetected;
    /// assert!(b.shape_is_consistent());
    /// ```
    #[must_use]
    pub fn shape_is_consistent(&self) -> bool {
        self.kind != SyncBlockerKind::TypeMismatchDir || self.side.is_some()
    }
}

/// The comparison options a sync plan EMBEDS (0.40.0, ADR 0049): the same
/// rungs, tolerance and depth as [`FsCompareParams`], without the two roots
/// — which in a plan are called `source` and `dest`.
///
/// It is a SEPARATE type and not a `flatten` of [`FsCompareParams`] on
/// purpose: that one is a method's request and is already published with
/// its shape. Restructuring it with `#[serde(flatten)]` changes the
/// SEMANTICS of its deserialization — a buffered map, a different path for
/// type errors — even though the JSON object looks the same, and that is
/// what this bump does not do. Adding it an OPTIONAL field, as
/// `descend_orphans` does in 0.40.0, is not the same: a 0.39 client's
/// request stays byte-for-byte 0.39's. See [`PROTOCOL_VERSION`]. The two
/// types have to move TOGETHER when the cascade gains a rung — the test
/// `las_dos_caras_de_las_opciones_de_comparacion_no_divergen` pins that.
///
/// It is called `Sync…` and not plainly `CompareOptions` because
/// `norte-compare` already has a `CompareOptions` that is NOT wire (it is
/// the engine's configuration), and `sync.plan`'s handler is going to have
/// both in front of it in the same file.
///
/// **Two of its fields are NOT the caller's in [`SYNC_PLAN`]**: see
/// `descend_orphans` and `follow_symlinks` below. They are present —
/// instead of omitted — precisely to be able to REJECT them: serde ignores
/// fields it does not know, so an absent field would turn "ask for it and
/// I say no" into "ask for it and nothing happens", which is the silently
/// overwritten value the design refuses.
///
/// ```
/// use norte_proto::methods::{CompareCriteria, SyncCompareOptions};
/// // Everything has a default: `{}` is a cheap whole-tree comparison.
/// let o: SyncCompareOptions = serde_json::from_str("{}").expect("options");
/// assert_eq!(o.criteria, CompareCriteria::default());
/// assert_eq!(o.mtime_tolerance_ms, 2000);
/// assert!(o.max_depth.is_none() && !o.follow_symlinks && o.descend_orphans.is_none());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncCompareOptions {
    /// Which rungs run. Absent = [`CompareCriteria::default`]. With `hash`
    /// on, [`SYNC_PLAN`] also requires CONTENT scope over both roots.
    pub criteria: CompareCriteria,
    /// Maximum descent depth, counting the root as 0. `None` = no limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// The mtime rung's tolerance in milliseconds. Default 2000 (the FAT
    /// rule). `u32` for the same reason as in
    /// [`FsCompareParams::mtime_tolerance_ms`].
    pub mtime_tolerance_ms: u32,
    /// Follow symlinks. In [`SYNC_PLAN`] **it is NOT the caller's**: sending
    /// it as `true` is `-32602`, not a value the core silently overwrites.
    /// Targets are compared AS BYTES, and planning copies through a
    /// followed link is a different thing nobody has designed.
    pub follow_symlinks: bool,
    /// Descend into directories that exist ONLY on this side. `None` — the
    /// default — emits one row for the orphan and does not walk it.
    ///
    /// In [`SYNC_PLAN`] **it is also NOT the caller's**: the planner fixes
    /// it to the SOURCE side, because whoever approves a plan needs how
    /// many files and how many bytes, and the executor needs one step per
    /// file to journal and to isolate a failure. On the DESTINATION an
    /// orphan is one whole [`SyncStepKind::DeleteTree`], and descending
    /// into it would buy forty thousand listings that do not change a
    /// single step. Asking for it is `-32602`.
    ///
    /// [`DescendSide`] and not [`Side`] for what that type explains: it is
    /// a request field, and a misspelled side has to die in the
    /// deserializer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub descend_orphans: Option<DescendSide>,
}

impl Default for SyncCompareOptions {
    fn default() -> Self {
        Self {
            criteria: CompareCriteria::default(),
            max_depth: None,
            mtime_tolerance_ms: default_mtime_tolerance_ms(),
            follow_symlinks: false,
            descend_orphans: None,
        }
    }
}

/// Params of [`SYNC_PLAN`] (0.40.0, ADR 0049).
///
/// They are called `source` and `dest`, never `left` and `right`: comparing
/// is symmetric and syncing is not, so the direction is translated ONCE, in
/// the frontend that knows which pane the user was in. Downstream it is
/// already a fact.
///
/// ```
/// use norte_proto::methods::{OnUnknown, SyncMode, SyncPlanParams};
/// // The MINIMUM: two roots and the mode. `mode` has no default — there is
/// // no neutral value between copying and deleting — everything else does.
/// let p: SyncPlanParams = serde_json::from_str(
///     r#"{"source":"file:///a","dest":"file:///b","mode":"update"}"#,
/// )
/// .expect("params");
/// assert_eq!(p.mode, SyncMode::Update);
/// assert_eq!(p.on_unknown, OnUnknown::Copy);
/// assert!(p.include.is_none());
/// assert!(serde_json::from_str::<SyncPlanParams>(r#"{"source":"file:///a","dest":"file:///b"}"#)
///     .is_err());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPlanParams {
    /// Where the bytes come from.
    pub source: VPath,
    /// Where they go. If it overlaps with `source` — equal, or one inside
    /// the other — the request is
    /// [`Error::OverlappingRoots`](crate::Error::OverlappingRoots) and no
    /// Task gets created.
    pub dest: VPath,
    /// `Update` or `Mirror`. WITHOUT a default, on purpose.
    pub mode: SyncMode,
    /// The options of the comparison underneath. Absent = everything by
    /// default. Two of its fields are not the caller's (see
    /// [`SyncCompareOptions`]).
    #[serde(default)]
    pub compare: SyncCompareOptions,
    /// What to do with what the provider cannot decide. Absent =
    /// [`OnUnknown::Copy`].
    #[serde(default)]
    pub on_unknown: OnUnknown,
    /// RELATIVE paths the plan is restricted to; absent = the whole tree.
    /// It is how the diff panel's first-class selection seeds a plan.
    ///
    /// More than [`SYNC_MAX_INCLUDE`] is a params error (`-32602`), not a
    /// trim: the same rule as [`FS_RENAME_BATCH_MAX_PAIRS`] and for the
    /// same reason — a silently shortened list syncs something nobody
    /// asked for.
    ///
    /// # What "restricted" exactly means
    /// Five rules, normative, because none of them follows from the
    /// sentence above:
    ///
    /// 1. **It restricts the plan's STEPS, not the comparison's rows.**
    ///    The tree is walked whole regardless: the spelling the
    ///    destination gives a folder travels in the folder's row, which is
    ///    usually `Same` and produces no step, so a plan that only looked
    ///    at the selection would compose destination paths with the
    ///    SOURCE's spelling.
    /// 2. **Naming a folder drags in its subtree**, by SEGMENT prefix.
    ///    `café` does not drag in `cafétière`.
    /// 3. **And the reverse, just enough:** a [`SyncStepKind::CreateDir`]
    ///    whose `rel` is an ancestor of something selected stays, even if
    ///    it was not named — without it the chosen copy would go to a
    ///    directory that does not exist. No other class gets dragged
    ///    upward: a [`SyncStepKind::DeleteTree`] on an ancestor would
    ///    delete exactly what was asked to be synced.
    /// 4. **The comparison is by BYTES**, over this same wire shape. It
    ///    does not normalize and does not case-fold, not even over a
    ///    filesystem that does: an NFC path does not match the same one in
    ///    NFD, and `README` does not match `readme`. A client must send
    ///    the bytes it saw, not a rewritten version of them. Also watch
    ///    out for steps whose `rel` is measured against the DESTINATION
    ///    ([`SyncStep::rel`]): a [`SyncStepKind::DeleteTree`] under a
    ///    folder the two sides write differently is not covered by a
    ///    selection taken from the source side.
    /// 5. **An EMPTY list is a selection of nothing**, not "everything": a
    ///    plan with no steps, `executable`. `include: [""]` (the root) IS
    ///    everything. Whoever does not want to filter OMITS the field.
    ///
    /// BLOCKERS are not filtered: see [`SyncPlanDone::executable`].
    #[cfg_attr(feature = "schema", schemars(extend("maxItems" = SYNC_MAX_INCLUDE)))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<RelPath>>,
}

/// What trash the DESTINATION of a plan has, and therefore what the undo
/// can return (0.40.0, ADR 0049). Daemon→client: `#[serde(other)]`.
///
/// # Why it travels, if every step already carries its [`StepReversal`]
///
/// Because `reversal` is NOT enough to decide the sentence a human needs
/// to read before approving. A [`SyncStepKind::Copy`] against a destination
/// WITHOUT a trash comes out with [`StepReversal::Delete`] — the step is
/// reversible wherever there is a trash, and marking it irreversible would
/// lie in the other direction — but that `created`'s undo also goes
/// through the trash (#65) and, there being none, it SKIPS it: the copy
/// stays. A copy-only plan against a destination without a trash and
/// another against a destination with a restorable trash are, step by
/// step, byte for byte, the SAME plan; and one undoes whole and the other
/// undoes nothing. Without this field there is no way to distinguish them,
/// and a dialog that plainly reads `reversal` promises what the undo is
/// not going to deliver.
///
/// # The three answers, and the difference between the two bad ones
///
/// - [`DestTrash::Restorable`] — there is a trash and it NAMES what it
///   buries (`Provider::trash_restorable`): the journal keeps its
///   `reversal_ref` and the undo CAN return the whole batch, copies
///   included. `file://` on Linux/BSD, and `sftp://`/object with a
///   logical trash.
/// - [`DestTrash::Opaque`] — there is a trash but it does not say
///   where it leaves things (`file://` on macOS and Windows). Every
///   step that ACTS comes out [`StepReversal::Irreversible`] with
///   [`SyncReason::NoTrashOnTarget`] (a [`SyncStepKind::Skip`] does not
///   act and keeps no reversal); what got buried still exists and can
///   be rescued BY HAND from the system's trash, but norte's undo
///   cannot guess which one it was.
/// - [`DestTrash::Absent`] — there is no trash. What gets overwritten
///   or deleted is nowhere, and what gets copied does not come back
///   either (the undo skips it).
///
/// # It is a promise about the PLAN, not a per-entry guarantee
///
/// [`DestTrash::Restorable`] says the destination knows how to name
/// what it buries, not that every entry is going to come back.
/// `Provider::trash_restorable` is a promise of the IMPLEMENTATION and
/// its own contract admits that a specific case may answer `None`; and
/// a path that changed between `sync.apply` and the undo gets BLOCKED
/// instead of touched. Both things end the same: the entry does not
/// come back and the undo's report NAMES it. A dialog can say "this
/// can be undone"; it cannot say "this is going to come back whole no
/// matter what".
///
/// The NET result of the last two is the same — the undo returns
/// nothing — and yet they are two different warnings: in one the file
/// exists and in the other it does not. That is why they are two
/// values and not a boolean.
///
/// ```
/// use norte_proto::methods::DestTrash;
/// assert_eq!(serde_json::to_string(&DestTrash::Opaque).expect("json"), r#""opaque""#);
/// // Only one of the three returns anything.
/// assert!(DestTrash::Restorable.restores());
/// assert!(!DestTrash::Opaque.restores() && !DestTrash::Absent.restores());
/// // And a value from a future daemon is NOT taken for any of the three.
/// let future: DestTrash = serde_json::from_str(r#""quantum""#).expect("degrades");
/// assert_eq!(future, DestTrash::Unknown);
/// assert!(!future.restores(), "what is not known makes no promise");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum DestTrash {
    /// A trash that names what it buries: the undo CAN return the whole
    /// batch, copies included. No per-entry guarantee — see the type's
    /// note.
    Restorable,
    /// A trash that does NOT name what it buries: nothing of the plan
    /// undoes, and what got buried gets rescued by hand from the system's
    /// trash.
    Opaque,
    /// No trash: nothing of the plan undoes, and what got destroyed is
    /// nowhere.
    Absent,
    /// A response this decoder does not know (`#[serde(other)]`). The core
    /// never emits it. **It promises nothing**: [`DestTrash::restores`] is
    /// `false`.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

impl DestTrash {
    /// The answer from the two facts the core measures of the
    /// destination's provider: whether it declares `CapabilityFlags::TRASH`
    /// and what `Provider::trash_restorable` promises.
    ///
    /// Written ONCE, here, because it is the translation of the SAME pair
    /// of booleans `norte_sync` uses to decide every step's
    /// [`StepReversal`]: if the two derivations drifted apart, the plan
    /// and its summary would say different things about the same
    /// destination.
    ///
    /// ```
    /// use norte_proto::methods::DestTrash;
    /// assert_eq!(DestTrash::of(true, true), DestTrash::Restorable);
    /// assert_eq!(DestTrash::of(true, false), DestTrash::Opaque);
    /// // With no trash, what the trash would promise decides nothing.
    /// assert_eq!(DestTrash::of(false, true), DestTrash::Absent);
    /// assert_eq!(DestTrash::of(false, false), DestTrash::Absent);
    /// ```
    #[must_use]
    pub fn of(has_trash: bool, restorable: bool) -> Self {
        match (has_trash, restorable) {
            (true, true) => Self::Restorable,
            (true, false) => Self::Opaque,
            (false, _) => Self::Absent,
        }
    }

    /// Does the undo of a plan applied over this destination return
    /// anything?
    ///
    /// `true` for [`DestTrash::Restorable`] and nothing else — the unknown
    /// variant included, which is not a promise but a gap.
    ///
    /// ```
    /// use norte_proto::methods::DestTrash;
    /// assert!(DestTrash::Restorable.restores());
    /// assert!(!DestTrash::Absent.restores());
    /// ```
    #[must_use]
    pub fn restores(self) -> bool {
        matches!(self, Self::Restorable)
    }
}

/// Payload of [`SYNC_PLAN_DONE`] (0.40.0, ADR 0049): what needs to be known
/// to approve a plan, and the hash it is approved with.
///
/// ```
/// use norte_proto::TaskId;
/// use norte_proto::methods::{DestTrash, PlanHash, SyncCounts, SyncPlanDone};
/// let d = SyncPlanDone {
///     task_id: TaskId::new(7),
///     plan_hash: PlanHash::parse(&"0".repeat(64)).expect("hex"),
///     counts: SyncCounts::default(),
///     blockers: vec![],
///     blockers_total: 0,
///     executable: true,
///     dest_trash: DestTrash::Restorable,
/// };
/// let json = serde_json::to_value(&d).expect("json");
/// // Empty `blockers` is an empty list, never an absent key.
/// assert_eq!(json["blockers"], serde_json::json!([]));
/// assert_eq!(json["plan_hash"], serde_json::json!("0".repeat(64)));
/// assert_eq!(json["dest_trash"], serde_json::json!("restorable"));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPlanDone {
    /// Owning Task (the same one [`SYNC_PLAN`] returned). It goes here
    /// because a connection may have two plans in flight, and until this
    /// notification arrives the client does not know the `plan_hash` to
    /// tell them apart.
    pub task_id: TaskId,
    /// The plan's hash, the ONLY thing [`SYNC_APPLY`] carries. Reuses
    /// [`PlanHash`] unchanged: [`PLAN_HASH_LEN`] lowercase hex characters,
    /// and a string of another shape dies at deserialization.
    pub plan_hash: PlanHash,
    /// What the plan adds up to, per step class.
    ///
    /// `counts.bytes` is a LOWER BOUND, not a total: steps whose size the
    /// provider did not give are counted in `counts.unmeasured_steps`
    /// instead of summing zero (see [`SyncCounts`], and
    /// [`SyncCounts::exact_bytes`] for the total or nothing). A dialog
    /// plainly showing `bytes` lies about almost any `file://` plan.
    pub counts: SyncCounts,
    /// The blockers, trimmed to [`SYNC_MAX_BLOCKERS_REPORTED`]. It is the
    /// EXPLANATION, not the verdict: what decides is `executable`.
    pub blockers: Vec<SyncBlocker>,
    /// How many blockers there REALLY were. No limit: 256 in the list and
    /// 40,000 here is an honest answer.
    pub blockers_total: u64,
    /// `true` when the plan can be executed as-is. NORMATIVE field, with
    /// the same direction as
    /// [`FsRenameBatchPlanResult::executable`]: the frontend disables
    /// confirming with `!executable` and deduces NOTHING from `blockers`,
    /// because a future blocker might have no name to list.
    ///
    /// INVARIANT (the core maintains it, a client may assume it):
    /// `blockers` non-empty ⟹ `executable == false`; and
    /// `executable == false` ⟹ [`SYNC_APPLY`] refuses with
    /// [`Error::PlanNotExecutable`](crate::Error::PlanNotExecutable) even
    /// if the hash matches.
    ///
    /// **[`SyncPlanParams::include`] does NOT trim the blockers**, so this
    /// always speaks of the WHOLE comparison. It is deliberate: some
    /// blockers' scope is the tree —
    /// [`SyncBlockerKind::DestReadOnly`] hangs off the root, which no
    /// selection names — and trimming them by selection would turn a
    /// read-only destination into an executable plan. The consequence a
    /// frontend has to know how to paint: a selection of three files can
    /// come back with `executable: false` because of something forty
    /// thousand rows away that the user does not have in front of them.
    pub executable: bool,
    /// What trash the DESTINATION has, meaning what this plan's undo can
    /// return if applied ([`DestTrash`]).
    ///
    /// **Without this field the approval dialog cannot be painted without
    /// lying**, and the reason is entirely in [`DestTrash`]'s rustdoc:
    /// every step's [`StepReversal`] does not distinguish a copy-only plan
    /// that undoes whole from an identical one that undoes nothing. It
    /// speaks of the WHOLE plan — it is a property of the destination's
    /// provider, not of a step — so [`SyncPlanParams::include`] does not
    /// affect it.
    ///
    /// Without `serde(default)` on purpose, for the same reason as
    /// [`SyncCounts`]'s new counters: a default would be an invented
    /// answer about whether something can be undone, and there is no
    /// published version that omits it (0.40.0 is the bump that debuts the
    /// whole family). [`DestTrash`] also does not derive `Default`, so
    /// giving it a `serde(default)` later does not silently compile: what
    /// gets invented has to be chosen by hand, which is exactly the
    /// decision that must not go unnoticed.
    ///
    /// Does not enter `plan_hash` and does not need to: it comes from
    /// `dest_has_trash` and `dest_trash_restorable`, which the hasher
    /// already seeds, so two plans with different trashes already have
    /// different digests. Meaning this field cannot contradict the plan
    /// that authorizes execution.
    ///
    /// **AFTER applying, the report is what rules.**
    /// [`SyncReportResult::dest_trash`] repeats this value (0.42.0, #170)
    /// so the report is self-sufficient, but it is that report's
    /// `batch_id` that says whether there is anything to undo: absent
    /// means no batch ever got opened, no matter what the plan promised.
    /// What the undo ended up saving is counted by
    /// `PolicyUndoReportResult`, with `skipped_created_no_trash` as the
    /// *after-the-fact* face of [`DestTrash::Absent`].
    pub dest_trash: DestTrash,
}

/// Params of [`SYNC_APPLY`] (0.40.0, ADR 0049): the hash, and NOTHING else.
///
/// ```
/// use norte_proto::methods::{PlanHash, SyncApplyParams};
/// let p = SyncApplyParams { plan_hash: PlanHash::parse(&"a".repeat(64)).expect("hex") };
/// let json = serde_json::to_value(&p).expect("json");
/// assert_eq!(json.as_object().expect("object").len(), 1, "there is no second parameter");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncApplyParams {
    /// The approved plan. Names a plan RETAINED server-side and bound to
    /// this connection; if it names no live one it is
    /// [`Error::PlanStale`](crate::Error::PlanStale).
    pub plan_hash: PlanHash,
}

/// Params of [`SYNC_REPORT`] (0.40.0, ADR 0049).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReportParams {
    /// The application's Task whose report is requested (the one from
    /// [`FsTaskResult::task_id`] that [`SYNC_APPLY`] returned).
    pub task_id: TaskId,
}

/// Why a step never happened (0.40.0, ADR 0049). Daemon→client:
/// `#[serde(other)]`.
///
/// ```
/// use norte_proto::methods::SyncFailureCause;
/// assert_eq!(
///     serde_json::to_string(&SyncFailureCause::Conflict).expect("json"),
///     r#""conflict""#
/// );
/// let future: SyncFailureCause = serde_json::from_str(r#""gremlins""#).expect("degrades");
/// assert_eq!(future, SyncFailureCause::Unknown);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum SyncFailureCause {
    /// The destination stopped resembling what the plan recorded. The
    /// revalidation `stat` caught it and NOTHING got written — it is the
    /// only safety net between the plan's TTL and a lost file.
    Conflict,
    /// The provider refused the write.
    ///
    /// It is an answer about PERMISSION, and that is why it is separate
    /// from [`SyncFailureCause::IllegalName`]: "you can't" and "it can't
    /// be named that" lead to different actions — asking for access, or
    /// fixing the name — and a report that mixed them would not serve
    /// either one.
    Denied,
    /// The name is not legal under the DESTINATION root.
    ///
    /// # Why it is an execution failure and not a plan blocker
    /// Nothing checks, while planning, that a name legal under the source
    /// is legal under the destination, and checking it would require
    /// modeling each filesystem's naming rules — which ones, and with
    /// what limits, is not in [`Capabilities`](crate::Capabilities). The
    /// cases are real: 86 `é` in NFC take up 172 bytes and 258 in NFD,
    /// which blows past `NAME_MAX`; `CON`, a trailing dot and a trailing
    /// space are not names on Windows; and `f:ads` writes an alternate
    /// data stream and "works".
    ///
    /// So it comes out here, with its own name. A generic
    /// [`SyncFailureCause::Io`] would have said "something broke" about
    /// the only family of failures the user can fix on their own, and the
    /// only one that will repeat identically on every attempt until they
    /// fix it.
    ///
    /// # It is BEST-EFFORT, and it is best not read as a guarantee
    /// It depends on the provider knowing how to tell "that name is
    /// invalid" apart from "something failed", and not all can: `file://`
    /// can (`InvalidFilename`, `EILSEQ`), but SFTP v3 answers a generic
    /// `Failure` to almost everything and object storage does not
    /// distinguish a too-long key from any other rejection — on those two,
    /// an illegal name arrives as [`SyncFailureCause::Io`]. This cause's
    /// absence does NOT prove the names were fine; its presence does prove
    /// one was not.
    IllegalName,
    /// The read or the write broke.
    Io,
    /// A cause this decoder does not know (`#[serde(other)]`). The core
    /// never emits it.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

/// ONE step that never happened (0.40.0, ADR 0049).
///
/// ```
/// use norte_proto::methods::{RelPath, SyncFailure, SyncFailureCause, SyncStepKind};
/// let f = SyncFailure {
///     rel: RelPath::parse_wire("viejo").expect("rel"),
///     dest_rel: None,
///     cause: SyncFailureCause::Denied,
///     kind: SyncStepKind::DeleteTree,
/// };
/// let json = serde_json::to_value(&f).expect("json");
/// assert_eq!(json["kind"], serde_json::json!("delete_tree"));
/// // And with the class in front, the row says which root its `rel`
/// // hangs from without anyone having to deduce it (0.42.0, #195).
/// assert!(!json.as_object().expect("object").contains_key("dest_rel"));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncFailure {
    /// Where, RELATIVE to the two roots and in BYTES, same as
    /// [`SyncStep::rel`].
    pub rel: RelPath,
    /// The DESTINATION path the step landed on, when it is not spelled
    /// like `rel` — the same field and the same rule as
    /// [`SyncStep::dest_rel`], repeated here because the report is read
    /// without the plan in front.
    ///
    /// Without it, the [`SyncFailureCause::IllegalName`] case gets told
    /// backward: a source's NFC `café/x.txt` whose folder the destination
    /// spells in NFD fails on name length — NFD takes more — and the
    /// report would show the NFC spelling, the short and legal one. "This
    /// name is not valid" pointing at a name that is valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_rel: Option<RelPath>,
    /// Why.
    pub cause: SyncFailureCause,
    /// WHAT step it was (0.42.0, #195): the same class it carried in the
    /// plan, [`SyncStep::kind`].
    ///
    /// The report is read WITHOUT the plan in front, and until 0.41.0 that
    /// was the difference between a step and a failure: [`SyncStep`]
    /// declares its class and [`SyncFailure`] threw it away, even though
    /// the core has it in hand when it builds the row. What got lost is
    /// **which root `rel` hangs from**. A [`SyncStepKind::DeleteTree`]
    /// always speaks of the DESTINATION; everything else that writes, of
    /// the source. Without the class, the only proof left on the wire was
    /// `dest_rel`, and nothing follows from its absence: a `DeleteTree`
    /// denied by permissions against a read-only destination — the most
    /// common hostile row of a [`SyncMode::Mirror`] — carries no
    /// `dest_rel` and its `rel` hangs from the destination. A panel that
    /// paints that path under the source column, or that decodes it with
    /// the encoding override of the tree that was not touched, is naming a
    /// destination subtree with the other side's codepage, on the screen
    /// that explains what got deleted.
    ///
    /// **Mandatory and without `serde(default)`**, for the same reason as
    /// [`SyncPlanDone::dest_trash`]: a default would be an invented class
    /// for a step that failed, and [`SyncStepKind::Unknown`] — the enum's
    /// `#[serde(other)]` — means "an N+1 daemon named a class this binary
    /// does not know", a different answer from "the emitter did not say".
    /// The N/N-1 window does not need it: a 0.41 daemon talking to a 0.42
    /// client does not negotiate (see [`version_compatible`]), and a 0.41
    /// client reading a 0.42 report ignores the extra key.
    ///
    /// **With one caveat about [`SyncStepKind::Unknown`]**, the one place
    /// in the wire where the core COULD emit it: a step whose class this
    /// binary cannot name fails with
    /// [`SyncFailureCause::Io`]-via-`Unsupported` and its class is copied
    /// as-is to this row. It does not happen today — the spool rejects an
    /// unknown-class step while READING it, so a plan with one never gets
    /// to execute — and if it happened it would mean "this binary read a
    /// plan it does not understand", not "the emitter did not say the
    /// class". A client treats it the same as a [`SyncStepKind::Unknown`]
    /// on a step: no anchor to assert.
    pub kind: SyncStepKind,
}

/// Result of [`SYNC_REPORT`] (0.40.0, ADR 0049): what the plan's
/// application did.
///
/// A failure is a ROW of the report, not the end of the Task: a step that
/// dies at file 40,000 of 500,000 gets noted and the Task keeps going,
/// just like the comparison turned its errors into rows.
///
/// ```
/// use norte_proto::methods::{DestTrash, SyncReportResult};
/// let r = SyncReportResult {
///     done: 3, failed: 0, skipped: 1, bytes: 4096,
///     failures: vec![], batch_id: Some(12), dest_trash: DestTrash::Restorable,
/// };
/// let json = serde_json::to_value(&r).expect("json");
/// assert_eq!(json["failures"], serde_json::json!([]));
/// assert_eq!(json["batch_id"], serde_json::json!(12));
/// // The report is self-sufficient: "can this be returned?" is answered
/// // with it in hand, without having kept the `sync.plan_done` (0.42.0, #170).
/// assert_eq!(json["dest_trash"], serde_json::json!("restorable"));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReportResult {
    /// Steps executed AND journaled.
    pub done: u64,
    /// Steps that failed. It has NO limit: `failures` lists the first
    /// ones, this number counts all of them.
    pub failed: u64,
    /// Steps the plan already had as [`SyncStepKind::Skip`], plus the ones
    /// cancellation left untried.
    pub skipped: u64,
    /// Bytes actually moved.
    ///
    /// This one IS exact: they are bytes written, counted as they are
    /// written. It does not have to match the plan's [`SyncCounts::bytes`],
    /// which is a lower bound because the listing does not always give
    /// sizes — over `file://` it almost never does.
    pub bytes: u64,
    /// The failures, trimmed to [`SYNC_MAX_FAILURES_REPORTED`]; `failed`
    /// is not trimmed.
    pub failures: Vec<SyncFailure>,
    /// The journal unit everything applied ended up under. An OPAQUE
    /// reference for the client: it serves to CITE it — in a log, in a
    /// notice, in a support report — not to interpret it, and no method
    /// accepts it as a parameter (undo is requested per session with
    /// [`POLICY_UNDO_SESSION`], and grouping by batch is the core's
    /// business).
    ///
    /// `None` ONLY when the application died before being able to open
    /// one — not when it did nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<i64>,
    /// What trash the DESTINATION had when this got applied, meaning what
    /// this batch's undo can return (0.42.0, #170).
    ///
    /// The SAME value that traveled in [`SyncPlanDone::dest_trash`], drawn
    /// from the same pair of plan options, and for the same reason:
    /// without it, "can this be undone?" has no answer. A copy-only plan
    /// against a destination without a trash and an identical one against
    /// one with a restorable trash are byte for byte the same report, and
    /// one undoes whole and the other undoes nothing (ADR 0049, #65).
    ///
    /// **It is here because the report is read without the `plan_done` in
    /// front.** Whoever applied it received it seconds earlier — they had
    /// to receive it to have the `plan_hash` — but a client that
    /// reconnected, that was not the one who planned, or that simply
    /// dropped the notification, could read what got copied, overwritten
    /// and deleted, and could not know whether any of it comes back.
    /// [`PolicyUndoReportResult`] answers the same thing *after the fact*,
    /// with `skipped_created_no_trash`, which is exactly the wrong moment:
    /// this informs a decision BEFORE it is made.
    ///
    /// Mandatory and without `serde(default)`, same as its
    /// [`SyncPlanDone`] twin and for the same reason — a default would be
    /// an invented answer about whether something can be undone.
    ///
    /// **It does NOT replace `batch_id`.** A [`DestTrash::Restorable`]
    /// with an absent `batch_id` still means no batch ever got opened and
    /// there is nothing to undo; the two fields answer different questions
    /// and have to be read together.
    pub dest_trash: DestTrash,
}

/// Params of [`TASK_CANCEL`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCancelParams {
    /// Task to cancel. Cancelling a terminal or nonexistent Task is not an
    /// error: the response arrives the same and the real state travels via
    /// [`TASK_PROGRESS`].
    pub task_id: TaskId,
}

/// Result of [`TASK_CANCEL`]: empty object, reserved for extension.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCancelResult {}

/// Params of [`TASK_PAUSE`] and [`TASK_RESUME`] (0.82.0).
///
/// ```
/// use norte_proto::TaskId;
/// use norte_proto::methods::TaskPauseParams;
/// let p = TaskPauseParams { task_id: TaskId::new(3) };
/// assert_eq!(serde_json::to_string(&p).unwrap(), r#"{"task_id":3}"#);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPauseParams {
    /// Task to pause or resume. A terminal or nonexistent one is not an
    /// error.
    pub task_id: TaskId,
}

/// Result of [`TASK_PAUSE`] and [`TASK_RESUME`]: empty object, reserved for
/// extension.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPauseResult {}

/// Params of [`TASK_MOVE`] (0.83.0).
///
/// ```
/// use norte_proto::TaskId;
/// use norte_proto::methods::TaskMoveParams;
/// let p = TaskMoveParams { task_id: TaskId::new(3), up: true };
/// assert_eq!(serde_json::to_string(&p).unwrap(), r#"{"task_id":3,"up":true}"#);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskMoveParams {
    /// Which task is moved.
    pub task_id: TaskId,
    /// Toward the front of the queue (`true`) or toward the back.
    pub up: bool,
}

/// Result of [`TASK_MOVE`]: empty object, reserved for extension.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskMoveResult {}

/// Params of [`CONNECTION_TRUST_HOST_KEY`] (TOFU flow, ADR 0015 D). Carries
/// the fingerprint the user VERIFIED; the core compares it against the key
/// the server presents again on retry, and only registers it if it
/// matches.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionTrustHostKeyParams {
    /// Host being connected to (`host`; the port separate).
    pub host: String,
    /// Port (absent = the scheme's default).
    #[serde(default)]
    pub port: Option<u16>,
    /// Key algorithm (e.g. `ssh-ed25519`).
    pub algo: String,
    /// Fingerprint in OpenSSH `SHA256:<base64>` format that the user
    /// confirmed (the same string `Error::HostKeyUnknown` carries).
    pub fingerprint: String,
}

/// Result of [`CONNECTION_TRUST_HOST_KEY`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionTrustHostKeyResult {
    /// `true` if the key ended up registered (idempotent: `true` also if
    /// it already was). `false` reserved for a future policy rejection.
    pub trusted: bool,
}

/// Params of [`CONNECTION_PROVIDE_SECRET`] (#325): the secret a human just
/// typed for a connection.
///
/// **`Debug` is hand-written and does NOT derive.** It is the protocol's
/// only type that carries secret material, and as soon as someone writes a
/// `tracing::debug!(?params)` in the RPC layer — exactly what one does to
/// debug a new method — a `derive` would have put the password in the log
/// file and the log panel. Same criterion as `norte_connect::Secret`,
/// which prints `Secret(***)`.
///
/// The secret lives in the daemon's memory for the session's duration and
/// is not written anywhere. The decision to let it cross the socket is in
/// ADR 0015 (2026-09-01 amendment).
///
/// ```
/// use norte_proto::methods::ConnectionProvideSecretParams;
/// let p = ConnectionProvideSecretParams {
///     conn: "rosetta".to_owned(),
///     secret: "hunter2".to_owned(),
/// };
/// // The day someone adds `Debug` to the `derive` above to fix something
/// // else, this turns red before the password reaches a log.
/// assert!(!format!("{p:?}").contains("hunter2"));
/// assert!(format!("{p:?}").contains("rosetta"));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionProvideSecretParams {
    /// Connection name in `connections.toml`, the same one
    /// [`crate::Error::SecretNeeded`] carried.
    pub conn: String,
    /// What was typed. Never logged, never persisted.
    pub secret: String,
}

impl std::fmt::Debug for ConnectionProvideSecretParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The connection name DOES show: without it, a `Debug` would help
        // debug nothing and someone would end up printing the whole struct
        // by hand.
        f.debug_struct("ConnectionProvideSecretParams")
            .field("conn", &self.conn)
            .field("secret", &"***")
            .finish()
    }
}

/// Result of [`CONNECTION_PROVIDE_SECRET`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionProvideSecretResult {
    /// `true` if the daemon stored it for this session. `false` reserved
    /// for a future policy rejection.
    pub stored: bool,
}

/// [`CONNECTION_FAILED`] notification (server→client): a remote connection
/// could NOT be established, and why (#322).
///
/// # Why it exists, if the error already travels
///
/// The failure reaches the caller as a taxonomy CATEGORY — almost always
/// `PermissionDenied` — and that is indistinguishable from a wrong key, a
/// mistyped passphrase or a bucket without permissions. The exact sentence
/// — "the secret for 'myconn' is defined but EMPTY" — was written to the
/// daemon's log and thrown away. With the embedded CLI it was readable,
/// because `tracing` comes out on the process's own stderr: the same
/// failure got diagnosed or not depending on the TRANSPORT, the worst way
/// for it to depend on something.
///
/// It travels as a notification and not inside the error on purpose. The
/// taxonomy deliberately carries no free text (see [`crate::Error`]):
/// whoever decides with the error decides by category, and a sentence to
/// read is not a category. Same shape as [`ConnectionDegraded`], this
/// repository's precedent for "a connection condition with a human
/// explanation".
///
/// A 0.63 peer does not know it and silently DISCARDS it, which is what
/// ADR 0004 mandates for an unknown notification. What it loses is exactly
/// what there was before: the failure keeps reaching it as a category,
/// without the sentence.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionFailed {
    /// Connection name from `connections.toml`, if the failure happened
    /// opening a named one. `None` for a typed URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conn: Option<String>,
    /// Scheme being connected to (e.g. `"sftp"`).
    pub scheme: String,
    /// Host, WITHOUT userinfo (rule 10).
    pub host: String,
    /// Cause, CLOSED vocabulary comparable by equality — like
    /// [`ConnectionDegraded`]'s `reason`. Current values: `"secret-missing"`,
    /// `"secret-empty"`, `"secret-not-utf8"`, `"secret-store"`,
    /// `"auth-rejected"`, `"no-user"`, `"agent"`.
    ///
    /// The set can GROW additively: whoever receives an UNKNOWN one
    /// gracefully degrades leaning on `detail`, never rejects the
    /// notification.
    pub reason: String,
    /// Human detail, already in the daemon's language. Presentation,
    /// NEVER contract: it is not parsed, not compared, and may be absent.
    ///
    /// Only filled in by the variants whose sentence is composed of fields
    /// norte itself sets — see `ConnectError::detalle_publico`. The ones
    /// wrapping third-party text, paths or the config file do NOT reach
    /// here: rule 10 does not distinguish between "a secret" and
    /// "something that may contain a secret".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// [`CONNECTION_DEGRADED`] notification (server→client): a remote session
/// was established with degraded security. Paths/host go REDACTED (rule
/// 10): `host` never carries userinfo.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionDegraded {
    /// The session's scheme (e.g. `"ftp"`).
    pub scheme: String,
    /// The session's host, WITHOUT userinfo (rule 10).
    pub host: String,
    /// Cause, CLOSED vocabulary comparable by equality (like
    /// `PolicyDenied.rule`). Current values: `"tls-auth-rejected"` (the
    /// server rejected `AUTH TLS` under `tls="allow"`; the session travels
    /// in the clear) and `"ftp-plaintext"` (FTP-via-plugin, ADR 0033: FTPS
    /// is debt, the session is ALWAYS in the clear). The set can GROW
    /// additively: a consumer receiving an UNKNOWN `reason` must
    /// gracefully degrade (generic "degraded session" message leaning on
    /// `detail`), never reject the notification.
    pub reason: String,
    /// Optional human detail (presentation, never contract).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Params of [`POLICY_REQUEST_SCOPE`] (M3-3b): an agent requests a scope.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestScopeParams {
    /// Requesting agent session (must match the connection's).
    pub session: String,
    /// Requested roots (containment by subtree prefix).
    pub roots: Vec<VPath>,
    /// Requested op-kinds (`copy|move|delete|mkdir|create`).
    ///
    /// `create` enters in 0.57.0 with [`FS_CREATE`]. A name the daemon
    /// does not recognize is DISCARDED without error (fail-closed), so a
    /// 0.57 client asking a 0.56 daemon for `create` gets a scope without
    /// that op and its creations get denied — not granted by accident.
    pub ops: Vec<String>,
    /// Requested TTL in milliseconds.
    pub ttl_ms: u64,
}

/// Result of [`POLICY_REQUEST_SCOPE`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestScopeResult {
    /// Request id, for a human to grant it with `policy.grant_scope`.
    pub request_id: u64,
}

/// Params of [`POLICY_GRANT_SCOPE`] (a human grants a pending request).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantScopeParams {
    /// Id returned by `policy.request_scope`.
    pub request_id: u64,
}

/// Result of [`POLICY_GRANT_SCOPE`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantScopeResult {}

/// [`POLICY_APPROVAL_REQUIRED`] notification (server→client): an `ask` op
/// awaits a decision. Paths go REDACTED if they carry userinfo (rule 10).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyApprovalRequired {
    /// Id to respond with `policy.decide`.
    pub approval_id: u64,
    /// Agent session that requested the op (if applicable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Op-kind (`copy|move|delete|mkdir|create`).
    ///
    /// Display ONLY: a client that does not recognize the value paints it
    /// as is and can still approve or deny, which is what the approval
    /// needs.
    pub op: String,
    /// Paths involved (wire, redacted). Display ONLY: never reparsed into
    /// an operation — the real op is tied server-side by `approval_id`.
    ///
    /// May be a PREFIX of the paths the decision covers: see `paths_total`.
    pub paths: Vec<String>,
    /// How many paths the decision truly covers (0.36.0). `0` = UNKNOWN (an
    /// N-1 server did not send it), and then it equals `paths.len()`.
    ///
    /// NORMATIVE for a frontend: if it is greater than `paths.len()`, the
    /// list is TRUNCATED and the human must be told. A batch of renames
    /// ([`FS_RENAME_BATCH`]) gates two paths per step and can bring
    /// thousands; the server trims what it broadcasts — the notification
    /// goes to every human connection and is retained for the TTL — but
    /// the DECISION is taken over all of them. A human who approves 32
    /// innocent-looking paths without knowing there were eight thousand is
    /// not consenting to what they think.
    #[serde(default)]
    pub paths_total: u64,
    /// The approval's TTL in milliseconds. `0` = UNKNOWN (e.g. a pending one
    /// rebuilt from `policy.pending`'s resync, which does not carry the
    /// remaining TTL): the frontend does not paint a countdown.
    pub ttl_ms: u64,
    /// What this op ADDS to the question (0.61.0, #314). Absent = the op
    /// and the paths answer it whole, which is the case for every other
    /// op.
    #[serde(default, skip_serializing_if = "ApprovalDetail::is_empty")]
    pub detail: ApprovalDetail,
}

/// What the op ADDS to the question, when the op and the paths do not
/// answer it (0.61.0, #314).
///
/// The approval carried the op and the paths, and for every other op that
/// IS the decision: approving "copy these twelve" is approving copying
/// those twelve. Changing permissions is the first op where two requests
/// with the SAME op and the SAME paths mean opposite things — `0600` and
/// `4777` — so without this field the human was not consenting to what
/// they think. Same argument [`PolicyApprovalRequired::paths_total`] makes
/// for the count.
///
/// A struct of optional fields and not an enum: so a future op adds its
/// own without touching what already travels, and a peer that does not
/// know it ignores it (ADR 0004). Every field is OPTIONAL and DISPLAY
/// only: the real operation lives server-side tied to `approval_id`, and
/// none of this is reparsed.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalDetail {
    /// The permissions about to be set, in `chmod(2)`'s twelve bits
    /// ([`MODE_PERMISSION_BITS`]). Only in a `set-mode`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    /// The change descends the TREE (0.62.0, #315). Only in a `set-mode`.
    ///
    /// Without this, `paths_total` lies by omission: a recursive over one
    /// root was asked as "set-mode over 1 path", and what was approved was
    /// a hundred thousand nodes. Same hole [`Self::mode`] came to close in
    /// 0.61 — a human who approves without seeing the scope is not
    /// consenting to what they think.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recursive: bool,
    /// The mode DIRECTORIES will carry, if different (0.62.0, #315).
    ///
    /// Separate from [`Self::mode`] because it is another permission over
    /// other things: approving `0644` without seeing that directories stay
    /// at `0777` is approving half the question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir_mode: Option<u32>,
}

impl ApprovalDetail {
    /// If it says nothing: a frontend does not paint an empty line for it.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.mode.is_none() && !self.recursive && self.dir_mode.is_none()
    }
}

/// Params of [`POLICY_DECIDE`] (a human approves/denies).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDecideParams {
    /// Id of the pending approval.
    pub approval_id: u64,
    /// `true` = approve, `false` = deny.
    pub approve: bool,
}

/// Result of [`POLICY_DECIDE`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDecideResult {}

/// A pending approval (element of [`PolicyPendingResult`]).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    /// Id to respond with `policy.decide`.
    pub approval_id: u64,
    /// Agent session that requested the op (if applicable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Op-kind.
    pub op: String,
    /// Paths involved (wire, redacted). Display ONLY: never reparsed into
    /// an operation — the real op is tied server-side by `approval_id`.
    ///
    /// May be a PREFIX: see `paths_total`.
    pub paths: Vec<String>,
    /// How many paths the decision covers (0.36.0). Same contract as
    /// [`PolicyApprovalRequired::paths_total`], including `0` = unknown.
    #[serde(default)]
    pub paths_total: u64,
    /// What the op adds to the question (0.61.0, #314). Same contract as
    /// [`PolicyApprovalRequired::detail`]: without it, a pending one
    /// rebuilt from the resync would show less than the notification that
    /// announced it.
    #[serde(default, skip_serializing_if = "ApprovalDetail::is_empty")]
    pub detail: ApprovalDetail,
}

/// Result of [`POLICY_PENDING`] (resync of pending approvals).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyPendingResult {
    /// Pending approvals.
    pub pending: Vec<PendingApproval>,
}

/// Params of [`RPC_CANCEL`] (#72): the id of the in-flight request to
/// cancel.
///
/// The `id` is the same type as [`crate::wire::RequestId`] — a number
/// (canonical emitter) or a string (JSON-RPC tolerance). It is not
/// validated against a map here (it is a best-effort notification): the
/// daemon checks it against its in-flight requests, and an id with no
/// match is a no-op.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcCancelParams {
    /// JSON-RPC id of the request to cancel.
    pub id: crate::wire::RequestId,
}

/// Cap on the rows [`JOURNAL_LIST`] returns in one response.
///
/// The same role as [`FS_LIST_MAX_PAGE`]: the client asks and the daemon
/// trims. Exists because a journal on a machine that has been working for
/// months has hundreds of thousands of entries, and an uncapped response is
/// the client's memory against the server's disk.
pub const JOURNAL_LIST_MAX_PAGE: u32 = 200;

/// Params of [`JOURNAL_LIST`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalListParams {
    /// Returns entries STRICTLY before this `seq`.
    ///
    /// `null` (or absent) is "from the newest", which is what a timeline
    /// sends when it opens. Not the same as a large number: the client has
    /// no reason to know what the last `seq` is, and forcing it to invent
    /// an upper bound would be asking it to guess the server's state.
    ///
    /// Paginated by `seq` and not by offset because `seq` is monotonic and
    /// never reused: a new entry written between two pages cannot displace
    /// ones already read nor hide one.
    #[serde(default)]
    pub before_seq: Option<i64>,
    /// How many rows AT MOST. A request, not a contract: the daemon trims
    /// to [`JOURNAL_LIST_MAX_PAGE`], and asking for more is not an error.
    pub limit: u32,
    /// Filters by actor class: `"user"`, `"agent"`, `"plugin"`… `null` is
    /// all of them.
    ///
    /// It is the class, not the identity: "what did session 7 do" is
    /// already asked elsewhere, and what a timeline needs to separate is
    /// what I did from what something did in my name.
    #[serde(default)]
    pub actor_kind: Option<String>,
}

/// A journal entry, as a timeline reads it ([`JOURNAL_LIST`]).
///
/// It does not carry the entry's hash nor the chain's: verifying is a
/// different question, with its own surface, and putting a hash here would
/// invite a client to believe that showing the list is having verified it.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalRow {
    /// Sequence number, monotonic and never reused. It is the entry's
    /// identity and what [`JOURNAL_UNDO_AFTER`] receives.
    pub seq: i64,
    /// When, in milliseconds since the epoch.
    pub ts_ms: i64,
    /// Who: `"user"`, `"agent"`, `"plugin"`…
    pub actor_kind: String,
    /// Which one, within that class (an agent's session). `null` for the
    /// human, who has no sessions to distinguish.
    #[serde(default)]
    pub actor_id: Option<String>,
    /// Which operation (`fs.copy`, `fs.move`…).
    pub op: String,
    /// About what, ALREADY SANITIZED for display.
    ///
    /// It is the wire form of a [`crate::VPath`] as it was stored, with
    /// characters dangerous to a terminal masked by the server
    /// (`norte_encoding::mask_terminal_hazards`, the same treatment as
    /// `fs.search`'s lines). A file name is chosen by whoever creates the
    /// file — including an agent inside its enclosure — and this is the
    /// screen where a human decides what to revert: a bidi override or an
    /// escape sequence here repaints the decision.
    ///
    /// Travels as text and not as `VPath` so that ONE unreadable entry — a
    /// tampered journal — does not bring down the whole page: a timeline
    /// missing a row is worse than one with a strange row, because a
    /// mutation that is not seen is indistinguishable from one that did
    /// not happen.
    ///
    /// **Do not parse it to act.** It is masked, so it may NOT be the real
    /// path; it is text to show. What identifies the entry is [`Self::seq`],
    /// which is what [`JOURNAL_UNDO_AFTER`] consumes.
    pub path: String,
    /// The destination, when the operation has two sides (move, rename).
    /// Sanitized the same as [`Self::path`].
    #[serde(default)]
    pub path_to: Option<String>,
    /// The text of [`Self::path`] (or [`Self::path_to`]'s) is painted
    /// DIFFERENT from what the stored bytes say: something had to be
    /// masked, or the bytes were not text.
    ///
    /// Travels with the row because already-sanitized text reads as
    /// faithful, and losing that distinction right here is losing it on
    /// the screen where what gets reverted is decided.
    #[serde(default)]
    pub hostile: bool,
    /// Whether this entry HAS an undo path (is not `Irreversible`).
    ///
    /// It is what the entry declared when it was written, not a promise
    /// that undoing it will work now: in between, the tree may have moved,
    /// and undo finds that out when it tries.
    ///
    /// A `reversal` token this daemon does not recognize counts as
    /// `false`: in a tampered journal, claiming something has an undo path
    /// is the expensive lie.
    pub reversible: bool,
    /// Whether this entry is the COMPENSATION of another — the `seq` that
    /// undid it.
    ///
    /// A compensation is a mutation that happened and so appears in the
    /// list, but [`JOURNAL_UNDO_AFTER`] does not undo it again: it is
    /// written with the actor of the human who ran the undo and with a
    /// real reversal, so without this field a client would count it as
    /// undoable and promise twice what is going to happen.
    #[serde(default)]
    pub undoes_seq: Option<i64>,
    /// Whether this entry is ALREADY undone: a compensation of it exists
    /// and is still alive.
    ///
    /// The server computes it with the SAME condition undo uses to
    /// choose — "compensated alive", not "compensated at some point",
    /// which is not the same when a batch's undo unwinds itself. A client
    /// cannot deduce it from the page it has: the compensation may be
    /// outside it.
    #[serde(default)]
    pub undone: bool,
    /// The batch it belongs to, if it is in one. Entries with the same
    /// `batch_id` are undone TOGETHER or not touched, so a timeline that
    /// painted them loose would offer a cut that does not exist.
    #[serde(default)]
    pub batch_id: Option<i64>,
}

/// Result of [`JOURNAL_LIST`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalListResult {
    /// The rows, from newest to oldest.
    pub rows: Vec<JournalRow>,
    /// What to send as [`JournalListParams::before_seq`] to keep going
    /// backward, or `null` when nothing older is left.
    ///
    /// The server computes it and not the client by subtracting one from
    /// the last `seq`: `seq`s are not dense — an entry may be missing — and
    /// that subtraction is exactly the kind of arithmetic that breaks the
    /// day they stop being so.
    ///
    /// **The end of the list is this field being `null`, and NOT "fewer
    /// rows came back than I asked for"**: the server trims to `limit` and
    /// does not say by how much, so counting rows does not distinguish "it
    /// ended" from "I gave you what your request allowed". A page that
    /// comes back exactly full right when the journal runs out offers a
    /// cursor and the next round answers empty: it is one round too many,
    /// and it is correct — the server cannot know nothing is left without
    /// looking.
    ///
    /// When continuing backward, [`JournalListParams::actor_kind`] must be
    /// THE SAME as the previous round's. Changing it midway is not an
    /// error and is not detected: the cursor is a `seq`, not a query, so
    /// what comes back are the other filter's rows from that point on, and
    /// the ones the new filter would have brought above it do not come
    /// back.
    #[serde(default)]
    pub next_before_seq: Option<i64>,
}

/// Params of [`JOURNAL_UNDO_AFTER`].
///
/// ```
/// use norte_proto::methods::JournalUndoAfterParams;
/// // Without a ceiling, the field does NOT travel: a 0.79 client sends exactly this.
/// let sin = JournalUndoAfterParams { seq: 4, upto_seq: None };
/// assert_eq!(serde_json::to_string(&sin).expect("json"), r#"{"seq":4}"#);
/// let con = JournalUndoAfterParams { seq: 4, upto_seq: Some(9) };
/// assert_eq!(
///     serde_json::to_string(&con).expect("json"),
///     r#"{"seq":4,"upto_seq":9}"#
/// );
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalUndoAfterParams {
    /// Undoes the human's actions with `seq` STRICTLY greater than this
    /// one.
    ///
    /// That is: the entry pointed to stays. It is what a human expects of
    /// "go back to here" pointing at a row — the pointed-to row is the
    /// state to return to, not the first victim.
    pub seq: i64,
    /// The CEILING (0.80.0): nothing with `seq` greater than this one gets
    /// undone.
    ///
    /// It is the newest `seq` the human had in front of them when the
    /// count was shown. Without a ceiling, whatever happened AFTER the
    /// list was painted — with the panel open, which is normal — entered
    /// the undo without having been counted: the question promised three
    /// and five got undone. `None` = no ceiling, which is what 0.79 did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upto_seq: Option<i64>,
}

/// Params of [`POLICY_UNDO_SESSION`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyUndoSessionParams {
    /// Agent session whose mutations get undone (same format as
    /// initialize's `agent_session`: `[A-Za-z0-9._-]`, 1..=64).
    pub session: String,
}

/// Result of [`POLICY_UNDO_SESSION`] **and of [`JOURNAL_UNDO_AFTER`]**: the
/// undo runs as a Task (progress via `task.progress`, cancellable with
/// `task.cancel`).
///
/// One type for two methods because both answer the same thing — "here is
/// your Task's id" — and are the SAME undo with a different selection
/// criterion: the same `TaskKind`, the same report via
/// [`POLICY_UNDO_REPORT`]. Inventing a second identical type would only
/// have given two places to get it wrong. What this forces one to
/// remember: a field added to it some day has to make sense for both, or
/// it has no place here.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyUndoSessionResult {
    /// The undo's Task.
    pub task_id: TaskId,
}

/// Params of [`POLICY_UNDO_REPORT`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyUndoReportParams {
    /// Undo Task whose report is requested (the one from
    /// [`PolicyUndoSessionResult::task_id`]).
    pub task_id: TaskId,
}

/// Result of [`POLICY_UNDO_REPORT`]: the undo's report. All zero and no
/// `blocked` = nothing to undo, ONLY if the Task ended `Completed`: a
/// `Failed`/`Cancelled` Task leaves PARTIAL counters with `blocked` absent
/// (the reason lives in its terminal `task.progress`) — the Task's state
/// is queried separately, this result does not carry it.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyUndoReportResult {
    /// Entries reverted successfully.
    pub undone: u64,
    /// `Irreversible` entries skipped (nothing to step on).
    pub skipped_irreversible: u64,
    /// `Created` reversals skipped because the provider has no trash (#65):
    /// the node STAYS at the destination — undoing it would have been a
    /// permanent delete and undo never destroys unrecoverably.
    pub skipped_created_no_trash: u64,
    /// `Created` reversals skipped because what is at that path **is not
    /// the node that entry created** (0.84.0, #369/#371, ADR 0152).
    ///
    /// The node stays and the session CONTINUES. It is an action by the
    /// reader — they put something else there — and not a divergence that
    /// forces stopping everything: if this blocked, editing a copied file
    /// with any editor that saves atomically — vim, VS Code, `sed -i`, all
    /// of them change the inode — would leave the whole copy undone for
    /// one file.
    ///
    /// It is a COUNTER and not a list, like its two neighbors above: the
    /// paths of what did not come back stay today inside the process and
    /// do not cross the wire — a wire `VPath` can carry userinfo (rule 10)
    /// and getting it out requires redacting it first. An N-1 client does
    /// not see this counter and reads an undo that reverted less than it
    /// expected without knowing why, which is what it saw BEFORE 0.84.0
    /// for any reason.
    #[serde(default)]
    pub skipped_not_ours: u64,
    /// First step where the (strict) LIFO stopped, if any. Undo goes from
    /// the NEWEST entry backward: what is after the block in the journal
    /// was already undone; what is BEFORE it in the journal (lower seq)
    /// was left NOT undone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<UndoBlocked>,
    /// **Undoing a BATCH of renames (0.36.0) got stuck halfway**: the
    /// executor could not revert an undo step it had already applied, so
    /// the directory did NOT go back to how it was.
    ///
    /// It is its own category and not a `blocked`: `blocked` says "I
    /// stopped here and the tree is consistent", and this says the exact
    /// opposite. When present, the Task ends `Failed` — an undo that said
    /// `Completed` would promise a restored tree that is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_stuck: Option<RenameStuckStep>,
    /// Reversals from a batch's undo that were APPLIED but whose
    /// compensation could not be written (0.36.0). Each one leaves an
    /// entry that keeps looking pending even though its effect already
    /// came back: a later undo will find it and get blocked there. It is
    /// the only signal of that.
    #[serde(default)]
    pub compensations_lost: u64,
    /// **Units the POLICY denied and undo skipped** (0.43.0, #171).
    ///
    /// It is not [`Self::blocked`], and reading them as the same would be
    /// reading the report backward: `blocked` says "I stopped here, the
    /// tree stayed consistent", and this says "this unit was not touched
    /// and undo continued with the rest". Each row carries the `seq` of
    /// its unit's first entry and the reason, in the same shape as a block
    /// because the reader's question is the same: what did not come back
    /// and why.
    ///
    /// Undo asks the policy unit by unit and INSIDE the Task (it used to
    /// do it all up front, on the caller's thread), so a scope that
    /// expires halfway is seen by the unit it falls on. It is the same
    /// rule as the forward executor: `Deny` is a report row, not a modal
    /// per step.
    ///
    /// Trimmed to [`UNDO_MAX_DENIED_REPORTED`]; [`Self::denied_total`]
    /// counts them all.
    #[serde(default)]
    pub denied: Vec<UndoBlocked>,
    /// How many units the policy denied, trimmed or not (0.43.0, #171).
    #[serde(default)]
    pub denied_total: u64,
}

/// Cap on the rows of [`PolicyUndoReportResult::denied`] the report LISTS
/// (0.43.0, #171); `denied_total` counts them all.
///
/// Same criterion as `sync`'s caps: an uncapped list travels the wire and
/// stays in the client's memory, and under a policy that denies by default
/// there would be one row per unit of the session.
pub const UNDO_MAX_DENIED_REPORTED: usize = 256;

/// An undo block: where and why (element of
/// [`PolicyUndoReportResult::blocked`]).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoBlocked {
    /// `seq` of the journal entry whose reversal blocked. OPAQUE reference
    /// for the client: it only makes sense against the server's
    /// journal/audit (M3-5) — it is for citing it, not for interpreting
    /// it.
    pub seq: i64,
    /// Reason (protocol error taxonomy; drift/conflict are typical).
    pub error: crate::Error,
}

/// An invocable plugin command (element of [`PluginInfo::commands`], P1): a
/// minimal, read-only mirror of the manifest's `command` contribution
/// (`CommandContrib` in `norte-plugin-host`) — what a frontend needs to
/// list the command (palette, extensions panel), not to run it. `title` is
/// text supplied by the plugin: NOT TRUSTED, a frontend must mask it
/// before rendering it (same treatment as `PluginInfo::name`).
///
/// ```
/// use norte_proto::methods::PluginCommandInfo;
/// let c: PluginCommandInfo =
///     serde_json::from_str(r#"{"id":"greet","title":"Greet"}"#).unwrap();
/// assert_eq!(c.id, "greet");
/// assert_eq!(c.title, "Greet");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginCommandInfo {
    /// Command id within the plugin (stable; passed together with the
    /// plugin's id to [`PLUGIN_RUN_COMMAND`], or to [`PLUGIN_RENAME_PLAN`]
    /// if it is a renamer).
    pub id: String,
    /// Readable title to display. Plugin text — NOT trusted.
    pub title: String,
    /// What it is (0.67.0, ADR 0095): a command that runs, or a RENAMER
    /// that proposes a plan. Absent on a 0.66 peer = command, which was
    /// the only thing there was; omitted when it is a command, so JSON
    /// from before does not move.
    #[serde(default, skip_serializing_if = "PluginCommandKind::is_command")]
    pub kind: PluginCommandKind,
}

/// What class of entry a [`PluginCommandInfo`] is (0.67.0, ADR 0095).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginCommandKind {
    /// Runs via [`PLUGIN_RUN_COMMAND`] and returns text.
    #[default]
    Command,
    /// Proposes a rename plan via [`PLUGIN_RENAME_PLAN`], reviewed and run
    /// like the AI's.
    Renamer,
    /// Proposes an ORGANIZE plan via [`PLUGIN_ORGANIZE_PLAN`] (0.77.0,
    /// phase 8): the same split as the renamer — proposes, does not
    /// mutate — with one more freedom, that the destination carries
    /// directories.
    ///
    /// It is a `kind` and not a new [`PluginInfo`] field for the same
    /// reason `renamer` was: an organizer is OFFERED where a command is
    /// offered (the palette), and separating it into another list would
    /// have given two places to look for the same question — "what does
    /// this plugin offer me". The wire exposure is identical to what
    /// 0.67.0 accepted: the field is omitted when it is `command`, so an
    /// old peer only sees this value if the plugin truly declares an
    /// organizer.
    Organizer,
}

impl PluginCommandKind {
    /// For `skip_serializing_if`: the always-there value does not travel.
    #[must_use]
    pub const fn is_command(&self) -> bool {
        matches!(self, Self::Command)
    }
}

/// A column a `columns` plugin contributes (element of
/// [`PluginInfo::columns`], 0.28.0, G3c, ADR 0037): discovery — WITH WHICH
/// `column_id` to call [`PLUGIN_COLUMN_VALUES`] and WHAT header to paint,
/// without the frontend having to guess by `category == "columns"`.
/// `header` is plugin text — NOT trusted, a frontend must mask it before
/// rendering it (same treatment as [`PluginCommandInfo::title`]).
///
/// ```
/// use norte_proto::methods::PluginColumnInfo;
/// let c: PluginColumnInfo =
///     serde_json::from_str(r#"{"id":"git-status","header":"Git"}"#).unwrap();
/// assert_eq!(c.id, "git-status");
/// assert_eq!(c.header, "Git");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginColumnInfo {
    /// Column id (the same `column_id` [`PLUGIN_COLUMN_VALUES`] expects).
    pub id: String,
    /// Readable header to display. Plugin text — NOT trusted.
    pub header: String,
}

/// A PANEL a plugin offers (0.74.0, phase 3): a layout slot whose content
/// the guest paints.
///
/// The `kind` is the manifest's; the slot ends up named
/// `plugin:<id>:<kind>`, which is what stops it colliding with a built-in
/// one. The minimums are declared by the plugin because it knows them, and
/// the layout collapses the slot when they do not fit, same as with any
/// other kind.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPanelInfo {
    /// Panel id within the plugin (the same `kind`
    /// [`PLUGIN_PANEL_RENDER`] expects).
    pub kind: String,
    /// Readable title. Plugin text — NOT trusted.
    pub title: String,
    /// Minimum width in cells, if the panel requests one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_cols: Option<u16>,
    /// Minimum height in cells, if the panel requests one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_rows: Option<u16>,
}

/// `true` if `id` is a valid reverse-DNS plugin identifier: one or more
/// `[A-Za-z0-9-]+` segments separated by dots, at least one dot, no empty
/// segment (no leading or trailing dot), and total length `1..=128`.
///
/// Lives NEXT TO [`PluginInfo`], which is where the id enters the process,
/// and not in the crate that parses manifests, because the question has
/// two entry points and one answer: the host asks it reading a
/// `plugin.toml`, and everyone who RECEIVES a `PluginInfo` over the wire
/// asks it again, because the process sending it is not trusted by
/// default. `norte-plugin-host` re-exports it so its own parsing keeps the
/// same name; two implementations of the same alphabet would be two, and
/// one would end up laxer.
///
/// The alphabet is this narrow on purpose: an id is NOT prose. It is a
/// search key against the catalogue, an argument of [`PLUGIN_HELP`] over
/// the wire, and what the help sidebar's filter folds on every keystroke.
/// With this alphabet it cannot paint terminal hazards nor impersonate
/// another plugin, and that is why an id that does not meet it is
/// DISCARDED instead of masked: masking is not injective, so it would map
/// two different plugins to the same row.
///
/// The 128 cap also bounds the work: a megabyte-long `id` costs one
/// rejected comparison, not one masked copy per plugin.
///
/// ```
/// use norte_proto::methods::is_valid_plugin_id;
///
/// assert!(is_valid_plugin_id("acme.ftp"));
/// assert!(!is_valid_plugin_id("acme"), "needs at least one dot");
/// assert!(!is_valid_plugin_id("acme.\u{202E}ftp"), "closed alphabet");
/// ```
#[must_use]
pub fn is_valid_plugin_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 128 {
        return false;
    }
    let mut segments = 0_usize;
    for segment in id.split('.') {
        if segment.is_empty()
            || !segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return false;
        }
        segments += 1;
    }
    // At least one dot ⇒ at least two segments.
    segments >= 2
}

/// A discovered plugin (element of [`PluginListResult::plugins`], M4-P3).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInfo {
    /// Stable plugin id (reverse namespace, e.g. `org.norte.demo`).
    pub id: String,
    /// Readable name to display.
    pub name: String,
    /// Publisher declared in the manifest.
    pub publisher: String,
    /// Plugin version (informational).
    pub version: String,
    /// Category (`previewer`, `indexer`…): what role it plays in the core.
    pub category: String,
    /// Capabilities the plugin requests (e.g. `fs-read`). A human approves
    /// them with [`PLUGIN_SET_APPROVAL`] before they take effect.
    pub capabilities: Vec<String>,
    /// `true` if a human already approved its capabilities.
    pub approved: bool,
    /// `true` if a human has it enabled.
    pub enabled: bool,
    /// Cosmetic description declared in the manifest (P1); absent = `None`.
    /// Does NOT form part of the approval digest (editing it does not
    /// invalidate already-approved capabilities) and is plugin text — NOT
    /// trusted, a frontend must mask it before rendering it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Commands the plugin exposes (P1); empty if it contributes none. An
    /// N-1 peer that builds its own `PluginInfo` does not emit this
    /// field — it is taken by its default (`vec![]`) when deserialized
    /// here.
    #[serde(default)]
    pub commands: Vec<PluginCommandInfo>,
    /// Columns the plugin contributes (0.28.0, G3c); empty if it
    /// contributes none. An N-1 peer that builds its own `PluginInfo` does
    /// not emit this field — it is taken by its default (`vec![]`) when
    /// deserialized here (same additive criterion as `commands` in
    /// 0.26.0).
    #[serde(default)]
    pub columns: Vec<PluginColumnInfo>,
    /// Panels the plugin paints (0.74.0, phase 3); empty if it contributes
    /// none. Same additive criterion as `columns` and `commands`: an N-1
    /// peer does not emit the field and it is taken here by its default
    /// (`vec![]`), i.e. "no panel to offer", which is what there was
    /// before the category.
    ///
    /// It is DISCOVERY, not a grant: it is listed without gating on
    /// approved or enabled, because what slots a plugin asks for is
    /// exactly what a human looks at BEFORE approving it — same as its
    /// commands and its columns.
    #[serde(default)]
    pub panels: Vec<PluginPanelInfo>,
    /// The manifest's AND the binary's approval anchor, as the daemon
    /// computes it NOW (0.53.0, #282). Hex sha256, or `None` on an old
    /// peer.
    ///
    /// It is what a human is looking at when deciding, so it is what
    /// [`PluginSetApprovalParams::expected_digest`] returns on confirming.
    /// Without it, a client can only compare the LIST of capabilities,
    /// which is what is painted and not what is granted:
    /// `category` and `contributions` — when and how the plugin fires —
    /// enter the anchor and not the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_digest: Option<String>,
    /// `true` if the plugin ships a SERVABLE `help.md` next to its
    /// `plugin.toml` (H3e, 0.34.0). It is cheap DISCOVERY: it decides
    /// whether the plugin's node appears in the help's topic sidebar, and
    /// avoids [`PLUGIN_HELP_MAX_BYTES`] per plugin travelling on every
    /// `plugin.list` — the content is requested separately with
    /// [`PLUGIN_HELP`], on demand.
    ///
    /// The host MUST compute it with the SAME guard it applies to serving
    /// [`PLUGIN_HELP`], not with a laxer existence check. Otherwise, the
    /// pair (`has_help: true`, `markdown: ""`) tells the caller "that path
    /// exists and is a regular file" about a file the host refuses to
    /// serve — a path oracle assembled from two open methods, neither
    /// gated by policy.
    ///
    /// What a RECEIVER can conclude, which is what matters here:
    ///
    /// - `true` does NOT promise a page with content. The host does not
    ///   read the file nor parse it, so an empty or unreadable `help.md`
    ///   comes out `true` and degrades when requested (empty markdown) —
    ///   the right direction, because the help is cosmetic and never
    ///   brings down a plugin.
    /// - `false` does NOT mean "there is no file". It means "there is no
    ///   page I am going to serve": it may not exist, or exist and not
    ///   pass the guard (a symlink that escapes the plugin's directory).
    ///   The two are indistinguishable on purpose, and distinguishing
    ///   them is exactly what would reopen the oracle. Whoever wants the
    ///   diagnostic gets it from `norte doctor`, locally, not from the
    ///   wire.
    ///
    /// `skip_serializing_if` on `false`: a plugin without help produces a
    /// payload IDENTICAL byte for byte to 0.33's (same strong additive
    /// criterion as 0.30's `attrs`). An N-1 peer that builds its own
    /// `PluginInfo` does not emit it and it is taken here as `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_help: bool,
}

/// A plugin directory that could NOT be loaded (element of
/// [`PluginListResult::errors`], M4-P3): reported for diagnostics, without
/// bringing down the rest of the catalogue.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginLoadError {
    /// Directory of the plugin that failed (display; may carry lossy
    /// bytes).
    ///
    /// It is the basename, never the absolute path: that would reveal the
    /// user's home to an agent calling `plugin.list`.
    ///
    /// Kept for compatibility and still what a client paints when
    /// [`Self::dir_bytes`] does not come. What it CANNOT do is say whether
    /// it was altered: the `to_string_lossy` that produces it puts
    /// `U+FFFD`, and `U+FFFD` is not a terminal hazard — it is Specials,
    /// neither control nor `Default_Ignorable` — so no receiver heuristic
    /// recovers it. That is why the field next to it exists (#265).
    pub dir: String,
    /// The basename's BYTES, as the OS gave them (0.53.0, #265).
    ///
    /// A plugin directory's name is bytes: on Linux `caf\xff` is a legal
    /// name, and `PluginLoadError.dir` used to arrive already converted
    /// with an UNMARKED `to_string_lossy`, so the error row declared
    /// itself faithful. With the bytes, the receiver does its own
    /// conversion and knows to mark it — the usual rule: what gets masked
    /// is stated.
    ///
    /// Additive: a 0.52 daemon does not emit it and the receiver falls
    /// back to [`Self::dir`], which is exactly what it did before. What is
    /// lost against the old one is the mark, not the name.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "label_wire")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Option<String>", extend("contentEncoding" = "base64"))
    )]
    pub dir_bytes: Option<Vec<u8>>,
    /// Readable reason for the failure (invalid manifest, unsupported
    /// version…).
    pub reason: String,
}

/// Params of [`PLUGIN_LIST`]: empty object, reserved for extension
/// (filters by category/state will arrive here as optional fields).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginListParams {}

/// Result of [`PLUGIN_LIST`]: the discovered catalogue and the load
/// failures.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginListResult {
    /// Discovered and loaded plugins (with their approved/active state).
    pub plugins: Vec<PluginInfo>,
    /// Directories that failed to load (best effort; see
    /// [`PluginLoadError`]).
    pub errors: Vec<PluginLoadError>,
}

/// Params of [`PLUGIN_SET_APPROVAL`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSetApprovalParams {
    /// Id of the plugin to (dis)approve.
    pub id: String,
    /// `true` = approve the capabilities, `false` = revoke.
    pub approved: bool,
    /// The anchor the human READ, if the client has it (0.53.0, #282).
    ///
    /// The daemon anchors the digest IT has at the moment of writing, not
    /// the one that was shown, so a different `plugin.toml` fits between
    /// the `plugin.list` the human saw and the confirming `set_approval`.
    /// Today that window is closed by ACCIDENT in the daemon — it
    /// discovers the catalogue once at startup — and is NOT closed in the
    /// embedded `Backend`, which rediscovers on every call.
    ///
    /// With this field the daemon refuses if it does not match, and what
    /// is granted is exactly what was read. Only applies to APPROVING:
    /// revoking grants nothing, and refusing a revocation over a stale
    /// digest would leave alive a permission someone is trying to remove.
    ///
    /// Additive: `None` is "the client does not send it", and then the
    /// daemon behaves like 0.52 — the check is what is lost against an old
    /// client, not correctness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_digest: Option<String>,
}

/// Result of [`PLUGIN_SET_APPROVAL`]: empty object, reserved for extension.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSetApprovalResult {}

/// Params of [`PLUGIN_SET_ENABLED`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSetEnabledParams {
    /// Id of the plugin to enable/disable.
    pub id: String,
    /// `true` = enable, `false` = disable.
    pub enabled: bool,
}

/// Result of [`PLUGIN_SET_ENABLED`]: empty object, reserved for extension.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSetEnabledResult {}

/// Params of [`PLUGIN_UNINSTALL`] (0.71.0, ADR 0104).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginUninstallParams {
    /// Id of the plugin to uninstall. The daemon validates it as a
    /// reverse-DNS id BEFORE turning it into a path: a `..` would be a
    /// delete outside `plugins/`.
    pub id: String,
}

/// Result of [`PLUGIN_UNINSTALL`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginUninstallResult {
    /// `true` if the plugin had consent when it was deleted: the report
    /// says so because it is what just stopped existing, and a frontend
    /// can warn that one installed later under the same id is born
    /// without it.
    pub was_approved: bool,
}

/// Params of [`PLUGIN_RUN_COMMAND`] (M4-P4).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRunCommandParams {
    /// Id of the plugin exposing the command.
    pub id: String,
    /// Name of the command to run (declared by the plugin).
    pub command: String,
    /// Command argument. Absent = `""` (the wire's default): a client
    /// that does not send it runs the command with no argument.
    #[serde(default)]
    pub arg: String,
}

/// Result of [`PLUGIN_RUN_COMMAND`]: the plugin command's output.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRunCommandResult {
    /// Output (string) the plugin command returns.
    pub output: String,
}

/// Params of [`PLUGIN_PREVIEW`] (M4-P5).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPreviewParams {
    /// Path of the file to preview (the core reads its bytes).
    pub path: VPath,
}

/// The preview a previewer plugin produces: the three fields go TOGETHER
/// (all-or-nothing). See [`PluginPreviewResult`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPreview {
    /// Id of the previewer plugin that produced the output.
    pub plugin_id: String,
    /// Readable name of the previewer plugin (for the "via …" indicator).
    pub plugin_name: String,
    /// Preview output (text).
    pub output: String,
    /// The host-side decoding of the file was LOSSY (0.29.0, #101): the
    /// core detected text in a non-UTF8 encoding and some byte was
    /// invalid, so the `�`s in the output come from the decoding, not the
    /// file. The frontend flags it next to the "via …" indicator (the raw
    /// viewer already flags its own `had_errors`; this gives preview mode
    /// the same honesty). Additive over 0.28.x: `#[serde(default)]` = an
    /// N-1 client/daemon that does not emit it reads as `false` (no
    /// warning, the safe direction). ALWAYS present when serializing (same
    /// "always-present additive" criterion as `PluginInfo::commands`).
    #[serde(default)]
    pub lossy: bool,
}

/// Result of [`PLUGIN_PREVIEW`] (M4-P5): the first matching previewer's
/// preview, or NOTHING. `flatten` over an `Option` makes the wire
/// `{plugin_id,plugin_name,output}` (matched) or `{}` (none); the Rust TYPE
/// makes a partial state UNBUILDABLE (the three fields go together in
/// [`PluginPreview`]), and a partial wire object collapses to `None` (no
/// preview, safe) — never a `plugin_id` without `output` (protocol-guardian
/// M4-P5). `None` = no previewer handles the mimetype; the frontend falls
/// back to the raw view.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPreviewResult {
    /// The preview, or `None` if no previewer matched.
    #[serde(flatten)]
    pub preview: Option<PluginPreview>,
}

/// A text span with optional styling (element of a
/// [`PluginPreviewStyled::lines`] line, 0.27.0, G3, ADR 0037): the HOST
/// paints, the plugin only describes. `text` is plugin text — NOT TRUSTED,
/// a frontend must mask it before rendering it (same treatment as
/// `PluginInfo::name`/`title`). `role` references a `norte_theme::Role`
/// name — the HOST validates it against the CLOSED set when producing it
/// (an unknown name never goes out on the wire as free text, it collapses
/// to `None` before serializing); a remote client that receives a `role`
/// it does not recognize from a daemon it does not fully trust must treat
/// it the same, as `None`. `fg` is a raw RGB color fallback for spans
/// without a role (e.g. a highlighter's fixed palette); when BOTH are
/// present, `role` wins — the user's theme takes precedence over a
/// plugin's fixed color. `bg` (0.66.0, D4) is the RGB background, painted
/// as is whether or not there is a role: together with `fg` it is what
/// lets an image previewer fit two pixels in one cell (`▀`). Absent on
/// every span before 0.66.0, and omitted from the wire when missing.
///
/// ```
/// use norte_proto::methods::SpanWire;
/// let s: SpanWire = serde_json::from_str(r#"{"text":"fn"}"#).unwrap();
/// assert_eq!(s.text, "fn");
/// assert_eq!(s.role, None);
/// assert_eq!(s.fg, None);
/// assert_eq!(s.bg, None);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanWire {
    /// The span's text. Plugin text — NOT trusted.
    pub text: String,
    /// `norte_theme::Role` role name (validated host-side; an unknown name
    /// never reaches here as `Some`, see the type's rustdoc).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Raw fallback RGB color when there is no `role` (one byte per
    /// channel).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<[u8; 3]>,
    /// The span's BACKGROUND color (0.66.0, D4). Exists for a single class
    /// of previewer: the one that paints an image with half-blocks, where
    /// each cell is TWO pixels — the top one in `fg`, the bottom one in
    /// `bg` — and without a background half the image does not exist. A
    /// frontend that does not paint backgrounds ignores it without losing
    /// text. A present `role` still wins over `fg`; over `bg` there is no
    /// role that wins, because the theme's roles are chrome and none of
    /// them describes a fragment's background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<[u8; 3]>,
}

/// The STYLED preview a previewer plugin produces (0.27.0, G3, ADR 0037):
/// twin of [`PluginPreview`] with `lines` of [`SpanWire`] instead of a
/// flat `output: String`. Wire caps (ADR 0037): ≤10,000 lines, ≤256 spans
/// per line, span text ≤4 KiB, total payload ≤4 MiB (the same runtime
/// return cap [`PLUGIN_PREVIEW`]/[`PLUGIN_RUN_COMMAND`] already use) —
/// the server applies them before sending; a client re-validates them and
/// falls back to [`PLUGIN_PREVIEW`] if they are violated.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPreviewStyled {
    /// Id of the previewer plugin that produced the output.
    pub plugin_id: String,
    /// Readable name of the previewer plugin (for the "via …" indicator).
    pub plugin_name: String,
    /// Preview lines; each line is an ordered list of spans.
    pub lines: Vec<Vec<SpanWire>>,
    /// The host-side decoding of the file was LOSSY (0.29.0, #101):
    /// identical to [`PluginPreview::lossy`] — the styled previewer
    /// receives the SAME text already decoded by the core, so it inherits
    /// the same warning.
    #[serde(default)]
    pub lossy: bool,
}

/// Params of [`PLUGIN_PREVIEW_STYLED`]: [`PluginPreviewParams`] (same file,
/// same previewer resolution) plus the viewer's width, which the flat
/// preview does not need because it cannot paint an image.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPreviewStyledParams {
    /// Path of the file to preview (the core reads its bytes).
    pub path: VPath,
    /// Width of the viewer that will paint the preview, in CELLS (0.66.0,
    /// D4). Exists because an image previewer has to decide how many
    /// cells to shrink the photo to, and only whoever paints knows. `None`
    /// = the client does not know or has no viewer (the CLI), and the
    /// guest picks its own default width. It is a HINT, not a contract: a
    /// guest may return longer lines and the viewer trims them as always.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<u32>,
}

/// Result of [`PLUGIN_PREVIEW_STYLED`]: the first matching previewer's
/// styled preview, or NOTHING. Same all-or-nothing `flatten`-over-`Option`
/// pattern as [`PluginPreviewResult`] (see its rustdoc): the wire is
/// `{plugin_id,plugin_name,lines}` (matched) or `{}` (none), never a
/// partial state.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPreviewStyledResult {
    /// The styled preview, or `None` if no previewer matched.
    #[serde(flatten)]
    pub preview: Option<PluginPreviewStyled>,
}

/// Parameters of [`PLUGIN_THUMBNAIL`] (0.73.0, ADR 0107).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginThumbnailParams {
    /// The file.
    pub path: VPath,
    /// The longer side the thumbnail must not exceed, in pixels. The
    /// plugin-host caps it to its own ceiling before calling the guest.
    pub max_edge: u32,
}

/// A thumbnail (0.73.0, ADR 0107): an encoded image, already verified by
/// the plugin-host — encoding among the ones the window paints, magic and
/// dimensions that match — with the plugin that made it.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginThumbnail {
    /// Id of the plugin that made it.
    pub plugin_id: String,
    /// Plugin name, PLUGIN TEXT: the frontend masks it.
    pub plugin_name: String,
    /// `image/png`, `image/jpeg` or `image/webp`.
    pub mimetype: String,
    /// The image's bytes, base64 on the wire. Capped to
    /// [`THUMBNAIL_WIRE_MAX_BYTES`] on deserializing: above that, the
    /// whole message is invalid.
    #[serde(with = "thumb_wire")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", extend("contentEncoding" = "base64"))
    )]
    pub bytes: Vec<u8>,
    /// Width in pixels, as the raster's header says.
    pub width: u32,
    /// Height in pixels, as the raster's header says.
    pub height: u32,
}

/// Cap on a thumbnail on the wire (ADR 0107): 4 MiB, the same the
/// plugin-host applies to the value a guest returns.
pub const THUMBNAIL_WIRE_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Result of [`PLUGIN_THUMBNAIL`]: the thumbnail, or nothing (`null`) if no
/// consented plugin matches or the one that matches did not know how.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginThumbnailResult {
    /// The thumbnail, or `None`.
    #[serde(flatten)]
    pub thumbnail: Option<PluginThumbnail>,
}

/// Cap on a panel frame's lines (0.74.0).
///
/// The caps live in the proto and not in each frontend because they are
/// part of the CONTRACT: two surfaces that trimmed differently would show
/// different panels for the same plugin, which is the divergence ADR 0077
/// pursues.
pub const PANEL_MAX_LINES: usize = 256;

/// Cap on spans per line of a panel frame (0.74.0).
pub const PANEL_MAX_SPANS_PER_LINE: usize = 256;

/// Cap on clickable zones of a panel frame (0.74.0).
pub const PANEL_MAX_HITS: usize = 128;

/// Cap on ONE span's text in a panel frame (0.74.0): 4 KiB.
///
/// The same as a styled preview's, and for the same reason: without it, a
/// 256×256-span frame has no size ceiling even with every count cap in
/// place. The host trims it on receiving it from the guest.
pub const PANEL_MAX_SPAN_TEXT: usize = 4 * 1024;

/// Cap on the OPAQUE state a panel keeps between repaints (0.74.0).
///
/// The host does not interpret it — it is not its own — but it does cap
/// it: a guest that wants to remember more than fits runs out of memory
/// between calls, which is its own problem and not the hosting process's.
pub const PANEL_MAX_STATE_BYTES: usize = 64 * 1024;

/// Why a panel is asked for a frame (0.74.0, phase 3).
///
/// The tag is `event` and NOT `kind`, which is the protocol's other two
/// tagged enums' tag. On purpose: `kind` already means "which of the
/// plugin's panels" two fields up in the same params, and a reader seeing
/// `"kind": "click"` next to `"kind": "git"` would have to stop and think
/// which is which. Whoever comes to align it with the house style is
/// changing the wire.
///
/// `#[non_exhaustive]` because it is going to grow — losing focus,
/// gaining it — and without it every downstream `match` turns a new
/// variant into a break.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PanelEvent {
    /// A repaint with no gesture behind it: the directory, the cursor or
    /// the slot's size changed.
    Refresh,
    /// Someone clicked inside the panel, at that cell of the FRAME.
    Click {
        /// Row within the frame, zero-based.
        row: u16,
        /// Column within the frame, zero-based.
        col: u16,
    },
    /// The keymap resolved a command while the panel had the keyboard.
    ///
    /// The command and not the key: a plugin does not bind chords on its
    /// own nor read what is typed elsewhere.
    Command {
        /// Id of the catalogue command.
        command: String,
    },
}

/// Parameters of [`PLUGIN_PANEL_RENDER`] (0.74.0, phase 3).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPanelRenderParams {
    /// Which plugin paints it.
    pub plugin_id: String,
    /// Which of its panels ([`PluginPanelInfo::kind`]).
    pub kind: String,
    /// The directory the panel accompanies: the listing under the
    /// keyboard.
    ///
    /// Travels so the guest knows WHERE it is; reading there still
    /// requires `norte:location`, with its consented prefix and its
    /// budget.
    pub dir: VPath,
    /// Slot width in cells.
    pub cols: u32,
    /// Slot height in cells.
    pub rows: u32,
    /// The reader's language (`es`, `en`), so the guest writes in it.
    ///
    /// Travels even though the host does not translate anything of the
    /// plugin's: what the panel paints is its own text, and without
    /// knowing the language it would always write in one — exactly what
    /// the help and the chrome stopped doing.
    pub lang: String,
    /// The name of the row under the cursor, already paintable, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_name: Option<String>,
    /// The opaque state the guest returned last time, if any.
    ///
    /// Travels as base64 and is CAPPED on deserializing to
    /// [`PANEL_MAX_STATE_BYTES`]: above that, the whole message is
    /// invalid. The cap goes here and not only on the guest's return
    /// because this is sent by a CLIENT — anyone who speaks the socket —
    /// and without it, returning a huge state would be enough for the
    /// daemon to decode it and copy it to the guest.
    #[serde(
        default,
        with = "panel_state_wire",
        skip_serializing_if = "Option::is_none"
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Option<String>", extend("contentEncoding" = "base64"))
    )]
    pub state: Option<Vec<u8>>,
    /// Why it is being called.
    ///
    /// FLATTENED over the params: on the wire it is `{"event": "click",
    /// "row": 2, "col": 5}` and not `{"event": {"event": "click", …}}`.
    /// Nested, the `event` key came out twice — the field and its tag are
    /// named the same — which is one of those things an implementer reads
    /// twice to believe.
    #[serde(flatten)]
    pub event: PanelEvent,
}

/// A clickable zone of a panel frame (0.74.0).
///
/// Names a catalogue COMMAND, never a free action: what a click in a
/// plugin panel can do is what the reader could do with a key, so the
/// policy does not widen for having panels.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelHit {
    /// Row within the frame, zero-based.
    pub row: u16,
    /// Column where it starts, zero-based.
    pub col: u16,
    /// How many cells wide it takes.
    pub width: u16,
    /// The catalogue command it runs.
    pub command: String,
    /// Its argument, if it carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg: Option<String>,
}

/// A panel frame already trimmed by the daemon (0.74.0).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelFrame {
    /// Id of the plugin that painted it.
    pub plugin_id: String,
    /// The lines, top to bottom.
    ///
    /// The span is the SAME type as a styled preview's ([`SpanWire`]), not
    /// a twin: this way the frontend paints it with the code it already
    /// has, and the protocol does not end up with two ways to say "this
    /// text is this color" — the second would always be the one someone
    /// forgets to validate the same way.
    pub lines: Vec<Vec<SpanWire>>,
    /// The clickable zones.
    #[serde(default)]
    pub hits: Vec<PanelHit>,
    /// The opaque state the guest wants for next time.
    ///
    /// Same treatment as on the way in
    /// ([`PluginPanelRenderParams::state`]): base64 with a cap on
    /// deserializing. The host does not interpret it — it is not its
    /// own — but it does cap it, in both directions.
    #[serde(
        default,
        with = "panel_state_wire",
        skip_serializing_if = "Option::is_none"
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Option<String>", extend("contentEncoding" = "base64"))
    )]
    pub state: Option<Vec<u8>>,
}

/// Result of [`PLUGIN_PANEL_RENDER`]: the frame, or NOTHING if there is no
/// panel to paint it, it is not consented, or the guest did not know how.
///
/// On the wire that is `{plugin_id, lines, …}` (there was a frame) or `{}`
/// (there was not): the field goes with `#[serde(flatten)]`, so "nothing"
/// is an EMPTY object and never `null` — same treatment as `plugin.preview`
/// and its styled twin. Whoever reads it comparing against `null` will
/// never see an absent frame.
///
/// Being missing is not an error: the slot keeps whatever last frame it
/// had and says so. A panel is cosmetic, and the cosmetic degrades.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPanelRenderResult {
    /// The frame, or `None`.
    #[serde(flatten)]
    pub frame: Option<PanelFrame>,
}

/// A thumbnail's bytes as base64 (0.73.0), capped on reading.
/// A panel's OPAQUE state on the wire (0.74.0): base64 with a cap on
/// reading.
///
/// Over `Option` because the state is missing the first time a panel is
/// painted, and that is not an error: a guest starting from scratch is
/// the normal case.
mod panel_state_wire {
    use serde::{Deserialize as _, Deserializer, Serializer};

    use crate::attrs::{decode_bytes_b64_lenient, encode_bytes_b64};

    #[expect(
        clippy::ref_option,
        reason = "serde `with` fixes the signature to the field's type, `&Option<Vec<u8>>`"
    )]
    pub(super) fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(bytes) => s.serialize_str(&encode_bytes_b64(bytes)),
            None => s.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        let Some(raw) = Option::<String>::deserialize(d)? else {
            return Ok(None);
        };
        let bytes = decode_bytes_b64_lenient(&raw)
            .ok_or_else(|| serde::de::Error::custom("panel state: invalid base64"))?;
        if bytes.len() > super::PANEL_MAX_STATE_BYTES {
            return Err(serde::de::Error::custom(
                "panel state: above the wire ceiling",
            ));
        }
        Ok(Some(bytes))
    }
}

mod thumb_wire {
    use serde::{Deserialize as _, Deserializer, Serializer};

    use crate::attrs::{decode_bytes_b64_lenient, encode_bytes_b64};

    #[expect(
        clippy::ptr_arg,
        reason = "serde `with` fixes the signature to the field's type, `&Vec<u8>`"
    )]
    pub(super) fn serialize<S: Serializer>(v: &Vec<u8>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&encode_bytes_b64(v))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let raw = String::deserialize(d)?;
        let bytes = decode_bytes_b64_lenient(&raw)
            .ok_or_else(|| serde::de::Error::custom("thumbnail bytes: invalid base64"))?;
        if bytes.len() > super::THUMBNAIL_WIRE_MAX_BYTES {
            return Err(serde::de::Error::custom(
                "thumbnail bytes: above the wire ceiling",
            ));
        }
        Ok(bytes)
    }
}

/// A git-status-like decoration of ONE entry (element of
/// [`PluginDecorations::decorations`], 0.27.0, G3, ADR 0037). `badge` is
/// plugin text — NOT trusted, ≤8 chars AFTER masking (wire cap, ADR 0037);
/// a frontend must mask (and truncate again) before trusting the cap the
/// server already applied — defense in depth. `role` follows the same
/// host-side validation as [`SpanWire::role`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecorationWire {
    /// Short badge (e.g. `"M"`, `"++"`). Absent = no badge for this entry
    /// from this plugin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    /// `norte_theme::Role` role name to paint the badge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// Params of [`PLUGIN_DECORATE`]: the VISIBLE entries of the current page
/// (batched — the frontend does not ask for decorations entry by entry).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDecorateParams {
    /// Paths to decorate, in listed order.
    pub paths: Vec<VPath>,
    /// Each path's class, POSITIONAL with `paths` (0.72.0, ADR 0105). An
    /// icon decorator needs it: a name does not say whether it is a
    /// directory. Empty (a 0.71 client) or short = `other` for what is
    /// missing, never an error: the class is cosmetic for the icon, not a
    /// batch condition. LONGER than `paths` IS `INVALID_PARAMS`: no
    /// correct client produces it, and silently truncating it would hide
    /// the error forever.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<EntryKind>,
}

/// Which SLOT of the row what a decorator returns is painted in (0.72.0,
/// ADR 0105). Declared by the manifest, carried by
/// [`PluginDecorations::slot`], and applied by the frontend: the two slots
/// coexist in a row, each one filled by the FIRST plugin for its slot.
///
/// A slot this build does not know falls back to `badge`, which is what a
/// newer peer can expect from an older one — and without this a new
/// `slot` would throw away the ENTIRE `plugin.decorate` response, with all
/// the page's decorations:
///
/// ```
/// use norte_proto::methods::DecorationSlot;
/// let s: DecorationSlot = serde_json::from_str("\"future_slot\"").unwrap();
/// assert_eq!(s, DecorationSlot::Badge);
/// let s: DecorationSlot = serde_json::from_str("\"icon\"").unwrap();
/// assert_eq!(s, DecorationSlot::Icon);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecorationSlot {
    /// To the left of the name, in a fixed-width column.
    Icon,
    /// To the right of the name: `M`, `++`. The usual slot, and the one a
    /// 0.71 client assumes when it does not see the field. `other` (which
    /// serde requires on the LAST variant) so that a slot this build does
    /// not know falls back here and does not break the batch.
    #[default]
    #[serde(other)]
    Badge,
}

impl DecorationSlot {
    /// For `skip_serializing_if`: the usual slot does not travel, so a
    /// badge decorator's wire is byte for byte 0.71's.
    ///
    /// ```
    /// use norte_proto::methods::{DecorationSlot, PluginDecorations};
    /// let mut d = PluginDecorations {
    ///     plugin_id: "org.norte.git".into(),
    ///     slot: DecorationSlot::Badge,
    ///     decorations: Vec::new(),
    /// };
    /// assert!(!serde_json::to_string(&d).unwrap().contains("slot"));
    /// d.slot = DecorationSlot::Icon;
    /// assert!(serde_json::to_string(&d).unwrap().contains("\"slot\":\"icon\""));
    /// ```
    #[must_use]
    pub fn is_badge(&self) -> bool {
        *self == Self::Badge
    }
}

/// The decorations of ONE `decorator` plugin (element of
/// [`PluginDecorateResult::plugins`]): `decorations` is POSITIONAL 1:1 with
/// `PluginDecorateParams::paths` — element `i` decorates path `i`, never a
/// key per path (cheap on the wire, and a hostile name cannot collide with
/// another as a key). A dead or failed plugin simply does not appear in
/// `plugins` (no decorations from THAT plugin; the rest of the page paints
/// the same — same fallback contract as [`PLUGIN_PREVIEW`]).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDecorations {
    /// Id of the `decorator` plugin that produced these decorations.
    pub plugin_id: String,
    /// The row slot they fill (0.72.0, ADR 0105). Absent = `badge`.
    #[serde(default, skip_serializing_if = "DecorationSlot::is_badge")]
    pub slot: DecorationSlot,
    /// Decorations, ONE per element of `paths` in the same order (an entry
    /// with no decoration from this plugin carries
    /// `DecorationWire{badge:None,role:None}`, never omitted — the index
    /// is the only link to the path).
    pub decorations: Vec<DecorationWire>,
}

/// Result of [`PLUGIN_DECORATE`]: the decorations from each approved,
/// enabled `decorator` plugin that responded.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDecorateResult {
    /// One element per `decorator` plugin that decorated this page.
    pub plugins: Vec<PluginDecorations>,
}

/// Params of [`PLUGIN_RENAME_PLAN`] (0.67.0, ADR 0095): which renamer of
/// which plugin, over which names of which directory.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRenamePlanParams {
    /// Which plugin, by reverse-DNS id: THAT one or none.
    pub plugin_id: String,
    /// Which of the renamers it declares ([`PluginCommandInfo::id`] with
    /// [`PluginCommandKind::Renamer`]).
    pub renamer_id: String,
    /// The names' directory: it is the location the guest can read if it
    /// was approved, and against which the plan will be checked
    /// afterward.
    pub dir: VPath,
    /// The names the batch acts on (what is marked, or pointed to), as
    /// text: a plan pair travels UTF-8, so a name that is not gets set
    /// aside beforehand and the client says so.
    pub names: Vec<String>,
}

/// Params of [`PLUGIN_ORGANIZE_PLAN`] (0.77.0, phase 8).
///
/// The same four fields as [`PluginRenamePlanParams`], with the ORGANIZER's
/// id instead of the renamer's: a plugin may declare several of each
/// class, and one specific one is requested.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginOrganizePlanParams {
    /// Which plugin, by reverse-DNS id: THAT one or none.
    pub plugin_id: String,
    /// Which of the organizers it declares.
    pub organizer_id: String,
    /// The names' directory, against which the plan is checked.
    pub dir: VPath,
    /// The names it acts on, as text and for the same reason as the
    /// renamer: what is not UTF-8 gets set aside beforehand.
    pub names: Vec<String>,
}

/// Params of [`PLUGIN_COLUMN_VALUES`]: the column id declared by the
/// `columns` plugin in its manifest, plus the visible paths to evaluate.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginColumnValuesParams {
    /// Column id (declared by the plugin; identifies WHICH column among
    /// several the same `columns` plugin might expose).
    pub column_id: String,
    /// WHICH plugin serves the column (0.35.0, #120). Absent = the host
    /// resolves by bare `column_id`, which is what it did before and
    /// keeps doing for a 0.34 client.
    ///
    /// The field exists because `column_id` does NOT identify the plugin
    /// and the host resolved to the first match: two consented plugins
    /// declaring the same bare id — `status` is the obvious example —
    /// made a column configured as `plugin:a/status` paint `b`'s values
    /// without anything saying so. The frontend ALWAYS knows which one
    /// the user configured (the configuration id carries the plugin
    /// inside), so what was missing was room on the wire to say it.
    ///
    /// A host that receives it MUST serve that plugin or none: if the
    /// named plugin is not approved, not enabled, or does not declare
    /// `column_id`, the response is absent cells — never another
    /// plugin's. Falling back to the first match would reintroduce the
    /// bug with one more field.
    ///
    /// `skip_serializing_if`: a request without this field is byte for
    /// byte 0.34's, so the N-1 window sees no new shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    /// Paths to evaluate, in listed order.
    pub paths: Vec<VPath>,
}

/// Result of [`PLUGIN_COLUMN_VALUES`]: `values` is POSITIONAL 1:1 with
/// `PluginColumnValuesParams::paths` — value `i` is path `i`'s cell. Each
/// cell is `Option<String>` (not `String`) for the SAME reason as
/// [`DecorationWire::badge`]: a column that does not apply to that entry
/// (e.g. "duration" over a file that is not media) needs to be
/// distinguished from a real value that happens to be the empty string —
/// `None` = no cell for this entry of this column, never omitted from the
/// positional vector (protocol-guardian, ADR 0037). Plugin text — NOT
/// trusted, a frontend must mask it before rendering it.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginColumnValuesResult {
    /// Cell values, one per element of `paths` in the same order; `None` =
    /// the column does not apply to that entry.
    pub values: Vec<Option<String>>,
}

/// Params of [`PLUGIN_GET_CONFIG`] (0.28.0, G3c).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginGetConfigParams {
    /// Id of the plugin whose `[config]` schema is queried.
    pub id: String,
}

/// Params of [`PLUGIN_HELP`].
///
/// ```
/// use norte_proto::methods::PluginHelpParams;
/// let p: PluginHelpParams = serde_json::from_str(r#"{"id":"acme.ftp"}"#).unwrap();
/// assert_eq!(p.id, "acme.ftp");
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginHelpParams {
    /// Id of the plugin whose `help.md` is requested. It is a SEARCH KEY
    /// against the catalogue: the host resolves it against the plugins it
    /// discovered and never composes it into a file path.
    pub id: String,
}

/// Result of [`PLUGIN_HELP`]: the capped `help.md` and what was lost
/// capping it.
///
/// ```
/// use norte_proto::methods::PluginHelpResult;
/// let r: PluginHelpResult =
///     serde_json::from_str(r#"{"markdown":"body","truncated":true,"lossy":false}"#)
///         .unwrap();
/// assert!(r.truncated && !r.lossy);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginHelpResult {
    /// The plugin's `help.md`, already capped and already VALID UTF-8 (the
    /// host decodes and substitutes what is unrecoverable). Without a
    /// readable `help.md`: an empty string, never an error — the help is
    /// cosmetic. That is why the ABSENT field is also accepted and read as
    /// that same empty page (`#[serde(default)]`): a peer expressing "no
    /// page" by omitting it cannot receive a deserialization failure for
    /// saying exactly what the contract already allows saying. On
    /// emission it is NEVER omitted (no `skip_serializing_if`), so absent
    /// and empty are only distinguished on INPUT, and there they mean the
    /// same thing.
    ///
    /// The cap is [`PLUGIN_HELP_MAX_BYTES`] and it caps THIS TEXT, not
    /// just the file's bytes: one byte can decode to three (windows-1252
    /// `0x80` → `U+20AC`), so a source that fit exactly would give triple
    /// the cap if only the source were capped. The host cuts both times
    /// and `truncated` covers both cuts, so a receiver can size by that
    /// constant and both sides see the SAME page.
    ///
    /// NOT masked: it carries verbatim any terminal hazards the plugin
    /// wrote (ESC, C0 controls, bidi overrides). It is parsed with
    /// `norte_help::parse_untrusted`, which masks when building the model;
    /// it is never painted or logged raw.
    #[serde(default)]
    pub markdown: String,
    /// The file exceeded the cap and was cut. Travels because the
    /// receiver CANNOT deduce it: the text arrives already short, so its
    /// own parsing would come out clean and the badge — the entire
    /// mitigation against a hostile `help.md` — would silently turn off.
    #[serde(default)]
    pub truncated: bool,
    /// Some byte did not decode under any reading and came out as
    /// `U+FFFD`. Travels for the same reason as `truncated`.
    #[serde(default)]
    pub lossy: bool,
}

/// A `[config.<key>]` key of a plugin's schema, schema + EFFECTIVE value
/// together (element of [`PluginGetConfigResult::keys`], 0.28.0, G3c, ADR
/// 0037): same "schema+value together" criterion that avoids a second
/// wire round trip to paint the settings UI. `kind` is CLOSED text
/// (`"string"|"bool"|"int"|"enum"` — the only four
/// `norte_plugin_host::ConfigKeySpec` declares); a frontend seeing an
/// unknown value (newer peer) must treat it as non-editable, never crash.
/// `default`/`value` ALWAYS travel as `String` (the SAME canonical
/// encoding as `norte_plugin_host::resolve_settings`: `bool` →
/// `"true"`/`"false"`, `int` → decimal), consistent with
/// [`PluginSetConfigParams::value`], also a `String`.
///
/// ```
/// use norte_proto::methods::PluginConfigKeyWire;
/// let k: PluginConfigKeyWire = serde_json::from_str(
///     r#"{"key":"greeting","kind":"string","default":"hello","value":"hello"}"#,
/// )
/// .unwrap();
/// assert_eq!(k.key, "greeting");
/// assert_eq!(k.kind, "string");
/// assert!(k.min.is_none());
/// assert!(k.values.is_empty());
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginConfigKeyWire {
    /// Key name (manifest's `[a-z0-9-]{1,32}` charset, safe to display as
    /// is — same criterion as `norte_plugin_host::is_valid_config_key`).
    pub key: String,
    /// Declared type: `"string"`, `"bool"`, `"int"` or `"enum"`.
    pub kind: String,
    /// Schema's default value, encoded as a canonical string.
    pub default: String,
    /// Inclusive lower bound (only `kind == "int"`). Absent = no bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    /// Inclusive upper bound (only `kind == "int"`). Absent = no bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    /// Allowed values (only `kind == "enum"`); empty for the other types —
    /// ALWAYS present (same additive criterion as `PluginInfo::commands`),
    /// never omitted.
    #[serde(default)]
    pub values: Vec<String>,
    /// Cosmetic description from the manifest. Plugin text — NOT trusted,
    /// a frontend must mask it before rendering it (same treatment as
    /// `PluginInfo::description`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Current EFFECTIVE value (schema defaults + `config.toml` already
    /// overlaid), encoded as a canonical string — the SAME encoding as
    /// `default`.
    pub value: String,
}

/// Result of [`PLUGIN_GET_CONFIG`]: the full schema + effective values, IN
/// THE MANIFEST'S KEY ORDER (same criterion as `PluginInfo::commands`:
/// manifest order, not reordered). An unknown `id` answers `keys: []` —
/// never an error (same indulgent criterion as `PLUGIN_LIST` with an
/// empty catalogue).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginGetConfigResult {
    /// One entry per declared `[config.<key>]` key.
    pub keys: Vec<PluginConfigKeyWire>,
}

/// Params of [`PLUGIN_SET_CONFIG`] (0.28.0, G3c): `value` is ALWAYS
/// `String` (the canonical encoding described in
/// [`PluginConfigKeyWire::value`]) — the daemon validates it against
/// `key`'s SCHEMA before persisting; it is never persisted unvalidated
/// (spec S2).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSetConfigParams {
    /// Id of the plugin whose setting changes.
    pub id: String,
    /// `[config.<key>]` key to set.
    pub key: String,
    /// New value, encoded as a canonical string (see
    /// [`PluginConfigKeyWire::value`]).
    pub value: String,
}

/// Result of [`PLUGIN_SET_CONFIG`]: empty object, reserved for extension.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSetConfigResult {}

/// `session.get` — the daemon's UI session (L2, 0.48.0): the layout and the
/// per-slot state the client left, so a daemon handoff (ADR 0055) does not
/// cost the screen.
///
/// The result also says whether THIS connection is the OWNER. The first
/// human connection that asks keeps it; the following ones receive a COPY
/// and run loose — same screen, same paths, and from there on they
/// diverge without writing. Opening a second terminal gives what the
/// reader expected and there are never two writers over one state.
///
/// ONLY human connections: an agent session has no screen to save. An
/// agent receives `INVALID_REQUEST`.
///
/// **Carries no params, and in 0.48 the server does not look at whatever
/// is sent.** A future params (e.g. "read without claiming") would not be
/// additive for that reason: a 0.48 daemon would IGNORE it and claim it
/// anyway, so whoever adds it has to do it with its own bump and its own
/// checkable method or field.
///
/// ```
/// assert_eq!(norte_proto::methods::SESSION_GET, "session.get");
/// ```
pub const SESSION_GET: &str = "session.get";

/// `session.put` — replaces the WHOLE session (L2, 0.48.0).
///
/// The complete blob travels and the client coalesces: the cursor moves on
/// every arrow key, and a family of per-field methods would be fifteen
/// methods, fifteen goldens and a merge engine nobody asked for.
///
/// [`SessionPutParams::revision`] is the whole concurrency story: a `put`
/// with a stale revision is rejected with [`crate::Error::Conflict`] and
/// the client re-reads. It is not for simultaneous editors — there are
/// none — but for the client reconnecting after a handoff with stale
/// state.
///
/// ```
/// assert_eq!(norte_proto::methods::SESSION_PUT, "session.put");
/// ```
pub const SESSION_PUT: &str = "session.put";

/// `session.release` — the OWNER connection relinquishes the UI session
/// (0.78.0, WOW program phase 9).
///
/// **Why it exists.** A handoff between frontends (`app.handoff`) is
/// dumping the screen, releasing it and launching the other, which claims
/// it in its `session.get`. Without this method, "releasing" only happened
/// on DISCONNECT, so the one leaving had to die before the one arriving
/// could claim it — and if the one arriving does not start, nobody is left
/// and the screen has gone with the one that died.
///
/// **Releasing someone else's does nothing**, and it is STATED
/// ([`SessionReleaseResult::released`]) instead of answering yes: one
/// connection does not evict another, and a lying `true` would leave the
/// caller believing it can claim something that still has an owner.
///
/// **Does not delete the body.** What is released is OWNERSHIP, not the
/// content: the document stays where it was and with its revision, which
/// is exactly what the other frontend is going to read. If nobody claims
/// it, the session stays ownerless and the first human who asks takes it —
/// the usual path.
///
/// ONLY human connections, like its two siblings: an agent has no screen
/// to release, and receives `INVALID_REQUEST`.
///
/// ```
/// assert_eq!(norte_proto::methods::SESSION_RELEASE, "session.release");
/// ```
pub const SESSION_RELEASE: &str = "session.release";

/// Cap on a session's `body`, in serialized bytes: 1 MiB.
///
/// The core checks it, which is the ONLY thing that can honestly be
/// checked about a document it does not read. Above it,
/// [`crate::Error::LimitExceeded`] and the stored session stays as it
/// was: a document whose schema is unknown is never truncated.
///
/// ```
/// assert_eq!(norte_proto::methods::SESSION_BODY_MAX, 1024 * 1024);
/// ```
pub const SESSION_BODY_MAX: usize = 1024 * 1024;

/// The UI session as it crosses the wire (L2).
///
/// `body` is OPAQUE to the core: `Node`, `SortSpec` and `ColumnId` live in
/// `norte-frontend`, which depends on this crate and not the other way
/// around, and mirroring them here would duplicate four types across a
/// dependency edge and turn every new UI field into a wire change with its
/// bump and its golden. Adding a field to the body is bumping the
/// `version` INSIDE the body, in the crate that gives it meaning.
///
/// ```
/// use norte_proto::methods::Session;
/// let s = Session {
///     version: 1,
///     revision: 3,
///     body: serde_json::json!({ "slots": {} }),
/// };
/// let j = serde_json::to_value(&s).expect("json");
/// assert_eq!(j["revision"], serde_json::json!(3));
/// // And a session nobody has written yet is revision zero:
/// assert_eq!(Session::default().revision, 0);
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
// Fields with a default, like the rest of the wire: a peer that omits one
// is missing a field, not adding an error.
#[serde(default)]
pub struct Session {
    /// `body`'s schema, owned by the frontends. 1 in this version; 0 in a
    /// session nobody has written yet.
    pub version: u32,
    /// Bumped by the core on every accepted `put`. 0 = session never
    /// written.
    pub revision: u64,
    /// The document. The core stores it, versions it and returns it; it
    /// does not read it.
    pub body: serde_json::Value,
}

/// Result of [`SESSION_GET`].
///
/// ```
/// use norte_proto::methods::{Session, SessionGetResult};
/// let r = SessionGetResult {
///     session: Session::default(),
///     owner: false,
/// };
/// let j = serde_json::to_value(&r).expect("json");
/// assert_eq!(j["owner"], serde_json::json!(false));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
// Fields with a default, like the rest of the wire: a peer that omits one
// is missing a field, not adding an error.
#[serde(default)]
pub struct SessionGetResult {
    /// The stored session, or an empty one with `revision: 0`.
    pub session: Session,
    /// `true` if whatever this connection writes is going to be SAVED: it
    /// is the owner, and the core serving it has where and the right to
    /// dump it.
    ///
    /// The two things are the same question for whoever reads this — "do
    /// my writes survive?" — and separating them only served to answer
    /// yes to a client that was going to lose it all on exit.
    pub owner: bool,
}

/// Params of [`SESSION_PUT`].
///
/// ```
/// use norte_proto::methods::SessionPutParams;
/// let p = SessionPutParams {
///     version: 1,
///     revision: 3,
///     body: serde_json::json!({}),
/// };
/// let j = serde_json::to_value(&p).expect("json");
/// assert_eq!(j["revision"], serde_json::json!(3));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
// Fields with a default, like the rest of the wire: a peer that omits one
// is missing a field, not adding an error.
#[serde(default)]
pub struct SessionPutParams {
    /// `body`'s schema as this client writes it.
    pub version: u32,
    /// The revision the client believes is current. Stale =
    /// [`crate::Error::Conflict`].
    pub revision: u64,
    /// The whole document.
    pub body: serde_json::Value,
}

/// Result of [`SESSION_PUT`]: the NEW revision.
///
/// ```
/// use norte_proto::methods::SessionPutResult;
/// let r = SessionPutResult { revision: 4 };
/// let j = serde_json::to_value(&r).expect("json");
/// assert_eq!(j["revision"], serde_json::json!(4));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
// Fields with a default, like the rest of the wire: a peer that omits one
// is missing a field, not adding an error.
#[serde(default)]
pub struct SessionPutResult {
    /// Resulting revision; the client keeps it for its next `put`.
    pub revision: u64,
}

/// Result of [`SESSION_RELEASE`] (0.78.0).
///
/// ```
/// use norte_proto::methods::SessionReleaseResult;
/// let r = SessionReleaseResult { released: true };
/// let j = serde_json::to_value(&r).expect("json");
/// assert_eq!(j["released"], serde_json::json!(true));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
// Fields with a default, like the rest of the wire: a peer that omits one
// is missing a field, not adding an error.
#[serde(default)]
pub struct SessionReleaseResult {
    /// `true` if this connection WAS the owner and has stopped being one.
    ///
    /// `false` is "it wasn't you", and it has to be distinguished:
    /// whoever hands off needs to know whether the session ended up free
    /// before launching the other frontend to claim it. Always answering
    /// yes would turn an impossible handoff into a window that opens and
    /// finds nothing.
    pub released: bool,
}

/// The CLOSED vocabulary of log levels that travels the wire, from least
/// to most verbose (0.65.0, #328).
///
/// It is here and not imported from `norte-config` because the
/// dependencies go the other direction: `norte-config` depends on this
/// crate, never the other way around. That they are the same five strings
/// is not a coincidence to maintain by hand — a `norte-config` test, the
/// only place both crates are visible from, checks that the two sets are
/// equal in BOTH DIRECTIONS. It is the same pattern this repository
/// already uses for the hashing vocabulary, where `norte-core` keeps a
/// frozen copy for the journal's format and an equality ties the two.
///
/// Compared by EQUALITY, so renaming any of the five is a wire change with
/// its bump. A receiver that sees one it does not know degrades — treats
/// it as the default level and says so — never rejects the response.
///
/// ```
/// use norte_proto::methods::LOG_LEVELS;
/// assert_eq!(LOG_LEVELS, ["error", "warn", "info", "debug", "trace"]);
/// // Least to most verbose: position is order, and a panel's filter is
/// // "at most this verbose".
/// assert_eq!(LOG_LEVELS[0], "error");
/// ```
pub const LOG_LEVELS: &[&str] = &["error", "warn", "info", "debug", "trace"];

/// `log.tail` — what the DAEMON's log ring has after a cursor (0.65.0,
/// #328).
///
/// # Why it exists
///
/// A frontend with a log panel paints its own process's ring, and that is
/// the correct answer only when the core is embedded. The window starts
/// its own daemon (#300), so its ring has the bridge's, the renderer's and
/// startup's lines, while the providers, the journal, the policy and why a
/// connection could not be opened happen on the other side of a socket.
/// The same happens to `ntc --socket`. The panel was not broken — since
/// #326 it says WHOSE ring it shows — but saying so is not the same as
/// being able to read the other one.
///
/// # Why it is PULLED and not pushed
///
/// The ring already carries a monotonic counter of lines that came in, so
/// a cursor costs nothing to produce, and with it **the daemon keeps no
/// per-client state**: there is no subscription to register nor drop to
/// lose when a client dies without warning. A notification that gets lost
/// along the way is a silent hole in a log, which is the worst thing that
/// can happen to a log — a missing line is indistinguishable from an event
/// that did not happen; a stale cursor, by contrast, is arithmetic, and
/// [`LogTailResult::lost`] says exactly how many lines fell off the back
/// before this cursor saw them. And a closed panel costs nothing, while a
/// subscription keeps paying.
///
/// What is paid in exchange is latency: up to one poll interval (~300 ms
/// in the frontend). For a list a person reads, that is not a cost.
///
/// # Who can call it
///
/// ONLY a human connection. An agent connection (`agent_session` in
/// [`INITIALIZE`]) receives `Error::PolicyDenied` with
/// `rule: "not-approved"`, which on the wire is
/// [`codes::APP_ERROR`](crate::wire::codes::APP_ERROR) (`-32000`) — the
/// same as `host.volumes`, `index.embed` or `ai.rename_plan`.
///
/// **It is NOT `INVALID_REQUEST` (`-32600`)**, and that has to be said here
/// because this rustdoc IS the published contract: a third-party client
/// that branched on `-32600` to recognize "forbidden" would never get it
/// right and would file the refusal as a generic application error —
/// which is to say, would confuse forbidden with empty again, exactly
/// what the next paragraph exists to separate.
///
/// The reason is concrete and not a principle: the daemon's ring carries
/// paths, connection names and OTHER sessions' activity, so for an agent
/// with a bounded scope it is an existence oracle over paths outside its
/// enclosure — exactly the leak `read_gate_all` already documents for
/// [`PLUGIN_DECORATE`] and [`PLUGIN_COLUMN_VALUES`]. An empty log and a
/// forbidden log cannot be read the same way, and that is why it is a
/// protocol error and not an empty list.
///
/// # When the other end does not have it
///
/// The reachable case is ONE: a daemon of the SAME version compiled
/// WITHOUT the `logging` feature, which has no ring to serve and answers
/// `Error::Unsupported` — [`codes::APP_ERROR`](crate::wire::codes::APP_ERROR)
/// (`-32000`), not `METHOD_NOT_FOUND`. The panel degrades to its local
/// ring and **says why**: the degradation has to be explicit, because a
/// panel left half-working without explanation looks broken.
///
/// A 0.64 daemon is NOT that case, and writing it here as if it were sends
/// whoever implements the degradation to write a dead branch: a 0.65
/// client never gets to send `log.tail` against it, because it dies
/// earlier in [`INITIALIZE`] with `VERSION_MISMATCH` (see
/// [`PROTOCOL_VERSION`]). There is no N/N-1 window to handle here.
///
/// ```
/// assert_eq!(norte_proto::methods::LOG_TAIL, "log.tail");
/// ```
pub const LOG_TAIL: &str = "log.tail";

/// `log.level` — raises the level the DAEMON's ring is keeping (0.65.0,
/// #328).
///
/// # Why it is a method and not a parameter
///
/// This is the design decision for the whole bump (ADR 0092). The ring has
/// a CAP that is not negotiable: `suppaftp` emits `PASS <password>` at the
/// `log` library's TRACE level (#43, rule 10), and this ring's level is
/// raised from the interface, so without the cap a keystroke on a panel
/// would put an FTP password on screen. The cap is an ALLOW list — only
/// norte's targets rise above INFO; everything third-party stays at INFO
/// no matter who asks for what.
///
/// That cap lives in the process that has the ring. For it to survive the
/// socket there are two ways, and only one works: the client ASKS for a
/// level and it is the daemon that calls its own setter, answering with
/// the level that truly ended up set. The other — sending the level as a
/// field the client applies — would require a second copy of the allow
/// list on the other side of the wire, and two copies of a defense drift
/// apart as soon as one changes.
///
/// # Two things that must be shown, not hidden
///
/// The level is **global to the daemon**: a client that raises it raises
/// it for everyone watching. And **it never lowers** — lowering to errors
/// and raising it again would show a gap the size of the time it stayed
/// low, and the gap is the lie this panel exists not to tell. A `level`
/// requested lower than the current one answers with the current one, and
/// that is not an error: it is the honest answer, and that is why the
/// result CARRIES the level instead of a `bool`.
///
/// # Who can call it
///
/// ONLY a human connection, per what [`LOG_TAIL`] says and more so:
/// raising the daemon's verbosity is raising that of a job an agent is not
/// part of. Same refusal and same code as there: `Error::PolicyDenied`
/// with `rule: "not-approved"`, i.e.
/// [`codes::APP_ERROR`](crate::wire::codes::APP_ERROR) (`-32000`), NOT
/// `INVALID_REQUEST`.
///
/// ```
/// assert_eq!(norte_proto::methods::LOG_LEVEL, "log.level");
/// ```
pub const LOG_LEVEL: &str = "log.level";

/// A log line AS IT TRAVELS (0.65.0, #328).
///
/// It is not the type a frontend paints, and that is on purpose.
/// `norte_config::logline::LogLine` is the PRESENTATION one: its level is
/// an enum that orders by verbosity — because that is the comparison the
/// filter makes — and it carries a label padded to five columns so the
/// list can be scanned by eye. Neither belongs on a wire: a column width
/// that changes cannot be a wire change, and an enum's order is not
/// serialized. Here the level is one of [`LOG_LEVELS`]'s strings,
/// comparable by equality and frozen by the goldens.
///
/// The two types coexist on purpose and the dependency only goes one way:
/// `norte-config` depends on this crate, so it is the one that converts.
///
/// ```
/// use norte_proto::methods::LogLine;
/// let l = LogLine {
///     epoch_ms: 1_756_000_000_000,
///     level: "warn".to_owned(),
///     target: "norte_core::connect".to_owned(),
///     message: "degraded session".to_owned(),
/// };
/// let j = serde_json::to_value(&l).expect("json");
/// assert_eq!(j["level"], serde_json::json!("warn"));
/// // `target` travels whole: it is what decides the security cap and what
/// // the reader filters by, so trimming it would break it both ways.
/// assert_eq!(j["target"], serde_json::json!("norte_core::connect"));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    /// Milliseconds since the epoch (UTC), same criterion as `mtime_ms`
    /// (ADR 0004): a SIGNED integer.
    ///
    /// It is what allows mixing the daemon's log with the process's own
    /// into a single list. The mix is honest as long as the two processes
    /// share a clock, which is the case for a local daemon; against a
    /// genuinely remote one it is not, and whoever does it has to say so
    /// instead of silently interleaving two clocks.
    pub epoch_ms: i64,
    /// The event's level: one of [`LOG_LEVELS`]'s strings.
    ///
    /// A string and not an enum because the vocabulary is closed and
    /// comparable by equality, and because an unknown value has to be
    /// able to ARRIVE: a receiver that does not recognize it degrades to
    /// the default level and says so, instead of rejecting a whole
    /// response over one line.
    pub level: String,
    /// Module that emitted it (`norte_core::connect`), whole.
    ///
    /// Whole because it is what decides the ring's security cap — what
    /// does not start with `norte` does not rise above INFO — and also
    /// what the reader filters by to keep one subsystem.
    pub target: String,
    /// The message and its fields, already flattened to text.
    ///
    /// It is PRESENTATION: not parsed, not compared, and may come in the
    /// daemon's language. What decides is in `level` and `target`.
    pub message: String,
}

/// Params of [`LOG_TAIL`].
///
/// ```
/// use norte_proto::methods::LogTailParams;
/// // A panel that just opened has no cursor: it wants whatever there is.
/// let opening = LogTailParams { cursor: None, max: 500 };
/// // The canonical emitter writes an explicit `cursor: null` (ADR 0004),
/// // which is exactly what makes the difference between "whatever you
/// // have" and "since the dawn of time" visible on the wire.
/// assert!(serde_json::to_string(&opening).unwrap().contains(r#""cursor":null"#));
/// // And the next round asks from where the previous one left off.
/// let following: LogTailParams =
///     serde_json::from_str(r#"{"cursor":1234,"max":500}"#).unwrap();
/// assert_eq!(following.cursor, Some(1234));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogTailParams {
    /// From where. It is the previous round's [`LogTailResult::next`], not
    /// an index into any list: it counts lines that have ENTERED the ring
    /// ever, which is the only thing that keeps meaning something once an
    /// old line has already fallen off the other end.
    ///
    /// **`null` (or absent) is "whatever you have"**, which is what a
    /// panel sends on opening, and is NOT the same as `0`. A zero asserts
    /// that the asker saw line number zero and wants everything after it,
    /// so against a ring that has already wrapped, the daemon would have
    /// to answer a huge `lost` — and that gap would be false: nobody lost
    /// lines they never expected. With `null` the daemon starts from the
    /// oldest it still keeps and answers `lost: 0`. That is why it is an
    /// `Option` and not a sentinel: a sentinel forces the two questions to
    /// share a value, and they are different questions.
    ///
    /// A cursor ABOVE what the daemon has seen — a daemon that restarted
    /// under a client that kept its own — is not an error nor a gap: it
    /// is answered with nothing new and nothing lost, because asserting a
    /// gap there would be lying in the other direction.
    #[serde(default)]
    pub cursor: Option<u64>,
    /// How many lines AT MOST in this response.
    ///
    /// It is a request, not a contract: the daemon trims, same as
    /// [`FS_LIST`] with [`FS_LIST_MAX_PAGE`] and [`FS_READ`] with
    /// [`FS_READ_MAX_CHUNK`]. Asking for more is not an error and nothing
    /// is lost — what does not fit stays after [`LogTailResult::next`],
    /// and the next round picks it up. To size it, see
    /// [`LogTailResult::capacity`], which is the upper bound on what can
    /// arrive (not necessarily the trim the daemon applies).
    ///
    /// **`0` is an error (`-32602`)**, the same criterion as
    /// [`FsListParams::limit`] with `Some(0)` and for the same reason: an
    /// empty page in a loop. A panel polling with `max: 0` would receive
    /// an empty list every round with the cursor not advancing, and on
    /// screen that reads as "nothing is happening" instead of the
    /// programming error it is. Rejecting it is what separates the two.
    ///
    /// And that is also why it carries no `default`: an absent `max`
    /// would be zero, i.e. the way to get it wrong would be exactly the
    /// same, only with nobody having written it.
    pub max: u32,
}

/// Result of [`LOG_TAIL`].
///
/// ```
/// use norte_proto::methods::{LogLine, LogTailResult};
/// let r = LogTailResult {
///     lines: vec![LogLine {
///         epoch_ms: 1_756_000_000_000,
///         level: "info".to_owned(),
///         target: "norte_core::daemon".to_owned(),
///         message: "listening".to_owned(),
///     }],
///     next: 4001,
///     lost: 12,
///     level: "info".to_owned(),
///     capacity: 2000,
/// };
/// let j = serde_json::to_value(&r).expect("json");
/// // Twelve lines fell off the back before this cursor saw them, and the
/// // response SAYS so instead of leaving a gap in the content.
/// assert_eq!(j["lost"], serde_json::json!(12));
/// assert_eq!(j["next"], serde_json::json!(4001));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogTailResult {
    /// The lines after the cursor, from OLDEST to newest.
    ///
    /// The order is part of the contract: whoever receives them appends
    /// them to the end of what they already had, and a reversed list
    /// would force every receiver to know it and flip it.
    pub lines: Vec<LogLine>,
    /// The cursor for the next call. Kept as is and sent as
    /// [`LogTailParams::cursor`].
    ///
    /// It is the position AFTER the last delivered line, not that of the
    /// last one: adding one to it externally is the classic bug that eats
    /// a line or repeats another, and here no arithmetic is needed.
    pub next: u64,
    /// How many lines fell off the ring before THIS cursor saw them.
    ///
    /// It is what makes polling honest. Without this number, a client
    /// falling behind — or that went a while without asking — would see a
    /// jump in the content and no explanation, and a log with a silent
    /// gap lies about what happened: a missing line is indistinguishable
    /// from an event that never happened. The panel already paints a gap
    /// marker for its local ring; this feeds the same marker for the
    /// remote one.
    ///
    /// **Not "everything the ring has ever discarded"**, which is a
    /// different number and says nothing about what the asker lost. It
    /// counts what THIS cursor lost, so with an up-to-date cursor it is
    /// zero, and for a first call with no cursor it is also zero.
    pub lost: u64,
    /// The level the daemon's ring is keeping RIGHT NOW: one of
    /// [`LOG_LEVELS`]'s strings.
    ///
    /// Travels on every response on purpose, instead of having its own
    /// method. Whoever paints the log has to show which level is set —
    /// otherwise "there are no DEBUG lines" is indistinguishable from "it
    /// is not being captured" — and the level is GLOBAL to the daemon:
    /// another client may have raised it a second ago. With a separate
    /// method, that response is born stale and the panel shows a level
    /// that is no longer the one there is; here it arrives with the lines
    /// it applies to, on the trip that was already being made.
    pub level: String,
    /// How many lines the daemon's ring can hold.
    ///
    /// Says how far back the history that can be requested reaches, which
    /// is what lets whoever paints it say "this is everything there is"
    /// instead of implying there is more.
    ///
    /// **It is an UPPER bound on what can arrive, not the trim the daemon
    /// applies.** No more lines than this will ever come, but quite a lot
    /// fewer may: a daemon with a 2000-line ring may be trimming to 500,
    /// and then a client asking `max: capacity` receives a short
    /// response. That is not a failure nor a loss — what did not fit
    /// stays after [`Self::next`] — but it does mean whoever needs to
    /// catch up has to **iterate over `next` until the response comes back
    /// empty**, and not assume that one call the ring's size brings it
    /// all.
    pub capacity: u32,
}

/// Params of [`LOG_LEVEL`]: the level the client REQUESTS.
///
/// ```
/// use norte_proto::methods::LogLevelParams;
/// let p = LogLevelParams { level: "debug".to_owned() };
/// assert_eq!(serde_json::to_value(&p).unwrap()["level"], serde_json::json!("debug"));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLevelParams {
    /// One of [`LOG_LEVELS`]'s strings.
    ///
    /// It is what is REQUESTED, not what results: the daemon applies its
    /// own cap and answers with what truly got set.
    ///
    /// **A value outside the vocabulary is `INVALID_PARAMS` (`-32602`)** and NOT
    /// degraded to a default level — accepting a level it does not
    /// understand and setting another would leave the reader believing it
    /// asked for something nobody did. Same code as [`LogTailParams::max`]'s
    /// `max: 0`, and for the same reason: it is a malformed request, not a
    /// missing capability.
    ///
    /// That it is this and not [`crate::Error::Unsupported`] matters
    /// because `Unsupported` already means something else in these two
    /// methods: "this daemon has no log to serve" (see [`LOG_TAIL`]). With
    /// a single code, a client could not distinguish a daemon with no ring
    /// from its own typo, which is the same confusion between empty and
    /// absent this bump exists not to have.
    pub level: String,
}

/// Result of [`LOG_LEVEL`]: the level that RESULTED.
///
/// It is not a "done" `bool`, and that is half the method's argument (see
/// [`LOG_LEVEL`]): the ring never lowers its level, so asking for one less
/// verbose than the current one answers with the current one, and that is
/// not a failure but the correct answer. With a `bool` one would have to
/// choose between lying with a `true` or alarming with a `false`.
///
/// ```
/// use norte_proto::methods::LogLevelResult;
/// // `warn` was requested with the ring already at `debug`: `debug` is
/// // answered, which is what there is. The ring does not lower (a gap the
/// // size of the time it stayed low is exactly what this panel exists not
/// // to have).
/// let r = LogLevelResult { level: "debug".to_owned() };
/// assert_eq!(serde_json::to_value(&r).unwrap()["level"], serde_json::json!("debug"));
/// ```
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLevelResult {
    /// The level in effect after the request: one of [`LOG_LEVELS`]'s
    /// strings. May be MORE verbose than requested.
    pub level: String,
}
