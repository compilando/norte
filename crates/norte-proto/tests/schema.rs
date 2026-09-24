//! Golden of the published protocol JSON Schema (ADR 0038, spec §11): it is
//! generated from the SAME serde types that speak the wire, so it cannot drift
//! from what the daemon actually sends. Break this test = wire-shape change =
//! regenerate with `NORTE_UPDATE_SCHEMA=1` (and bump + double review).
#![cfg(feature = "schema")]

use std::path::Path;

use norte_proto::methods::*;
use norte_proto::*;

/// Aggregate root: one field per top-level wire type. `schema_for!` emits each
/// as a property and pulls every nested type into `$defs`, so the single
/// document covers the whole protocol surface. Field values are never read.
#[derive(schemars::JsonSchema)]
#[allow(dead_code)]
struct ProtocolSchema {
    ai_rename_entry: AiRenameEntry,
    archive_format: methods::ArchiveFormat,
    archive_pack_params: methods::ArchivePackParams,
    archive_pack_report_params: methods::ArchivePackReportParams,
    archive_pack_report_result: methods::ArchivePackReportResult,
    pack_risky_name: methods::PackRiskyName,
    archive_test_failure: methods::ArchiveTestFailure,
    archive_test_params: methods::ArchiveTestParams,
    archive_test_report_params: methods::ArchiveTestReportParams,
    archive_test_result: methods::ArchiveTestResult,
    file_combine_params: methods::FileCombineParams,
    file_split_params: methods::FileSplitParams,
    ai_rename_plan_params: AiRenamePlanParams,
    ai_rename_plan_result: AiRenamePlanResult,
    attr_hint: AttrHint,
    attr_info: AttrInfo,
    attr_type: AttrType,
    attr_value: AttrValue,
    byte_range: ByteRange,
    capabilities: Capabilities,
    capability_flags: CapabilityFlags,
    client_info: ClientInfo,
    collision_policy: CollisionPolicy,
    compare_confidence: CompareConfidence,
    compare_criteria: CompareCriteria,
    compare_criterion: CompareCriterion,
    compare_reason: CompareReason,
    compare_row: CompareRow,
    compare_rows_batch: CompareRowsBatch,
    compare_verdict: CompareVerdict,
    conflict_kind: ConflictKind,
    connection_degraded: ConnectionDegraded,
    connection_failed: ConnectionFailed,
    connection_close_params: ConnectionCloseParams,
    connection_close_result: ConnectionCloseResult,
    connection_trust_host_key_params: ConnectionTrustHostKeyParams,
    connection_trust_host_key_result: ConnectionTrustHostKeyResult,
    connection_provide_secret_params: ConnectionProvideSecretParams,
    connection_provide_secret_result: ConnectionProvideSecretResult,
    daemon_going_away: DaemonGoingAway,
    daemon_shutdown_params: DaemonShutdownParams,
    daemon_shutdown_result: DaemonShutdownResult,
    decoration_slot: DecorationSlot,
    decoration_wire: DecorationWire,
    delete_mode: DeleteMode,
    descend_side: DescendSide,
    dest_trash: DestTrash,
    entry: Entry,
    entry_kind: EntryKind,
    error: Error,
    fs_capabilities_params: FsCapabilitiesParams,
    fs_capabilities_result: FsCapabilitiesResult,
    fs_compare_params: FsCompareParams,
    fs_copy_params: FsCopyParams,
    fs_delete_params: FsDeleteParams,
    fs_dir_size_params: FsDirSizeParams,
    fs_checksum_params: FsChecksumParams,
    fs_checksum_report_params: FsChecksumReportParams,
    fs_checksum_report_result: FsChecksumReportResult,
    fs_dir_usage_params: FsDirUsageParams,
    fs_dir_usage_report_params: FsDirUsageReportParams,
    fs_dir_usage_report_result: FsDirUsageReportResult,
    dir_usage_child: DirUsageChild,
    checksum_entry: ChecksumEntry,
    checksum_algo: ChecksumAlgo,
    checksum_miss: ChecksumMiss,
    fs_list_params: FsListParams,
    fs_list_result: FsListResult,
    fs_mkdir_params: FsMkdirParams,
    fs_create_params: FsCreateParams,
    fs_set_mode_params: FsSetModeParams,
    fs_move_params: FsMoveParams,
    fs_read_params: FsReadParams,
    fs_read_result: FsReadResult,
    fs_rename_batch_params: FsRenameBatchParams,
    fs_rename_batch_plan_params: FsRenameBatchPlanParams,
    fs_rename_batch_plan_result: FsRenameBatchPlanResult,
    fs_rename_batch_report_params: FsRenameBatchReportParams,
    fs_rename_batch_report_result: FsRenameBatchReportResult,
    fs_search_params: FsSearchParams,
    fs_stat_params: FsStatParams,
    fs_stat_result: FsStatResult,
    fs_task_result: FsTaskResult,
    grant_scope_params: GrantScopeParams,
    grant_scope_result: GrantScopeResult,
    host_volumes_params: HostVolumesParams,
    host_volumes_result: HostVolumesResult,
    connection_entry: ConnectionEntry,
    connection_list_result: ConnectionListResult,
    index_build_params: IndexBuildParams,
    index_build_result: IndexBuildResult,
    index_embed_params: IndexEmbedParams,
    index_hit: IndexHit,
    index_query_params: IndexQueryParams,
    index_query_result: IndexQueryResult,
    index_search_semantic_params: IndexSearchSemanticParams,
    index_search_semantic_result: IndexSearchSemanticResult,
    initialize_params: InitializeParams,
    initialize_result: InitializeResult,
    log_level_params: LogLevelParams,
    log_level_result: LogLevelResult,
    log_line: LogLine,
    log_tail_params: LogTailParams,
    log_tail_result: LogTailResult,
    match_info: MatchInfo,
    on_unknown: OnUnknown,
    pair_transform: PairTransform,
    pending_approval: PendingApproval,
    plan_hash: PlanHash,
    plugin_column_info: PluginColumnInfo,
    plugin_column_values_params: PluginColumnValuesParams,
    plugin_column_values_result: PluginColumnValuesResult,
    plugin_command_info: PluginCommandInfo,
    plugin_command_kind: PluginCommandKind,
    plugin_config_key_wire: PluginConfigKeyWire,
    plugin_decorate_params: PluginDecorateParams,
    plugin_decorate_result: PluginDecorateResult,
    plugin_rename_plan_params: PluginRenamePlanParams,
    plugin_decorations: PluginDecorations,
    plugin_get_config_params: PluginGetConfigParams,
    plugin_get_config_result: PluginGetConfigResult,
    plugin_help_params: PluginHelpParams,
    plugin_help_result: PluginHelpResult,
    plugin_info: PluginInfo,
    plugin_list_params: PluginListParams,
    plugin_list_result: PluginListResult,
    plugin_notice: PluginNotice,
    // `PanelEvent` is not reachable either, and for the same reason as
    // `PanelFrame`: it goes FLATTENED inside the params, and the generator
    // does not pull in what `flatten` hides. Without this line the artifact
    // would publish a method whose request it does not fully describe.
    panel_event: methods::PanelEvent,
    // No `panel_span`: a panel span is a `SpanWire`, the same one a styled
    // preview uses, and that one is already in the artifact.
    //
    // `PanelFrame` is not reachable from the result: it travels with
    // `#[serde(flatten)]` inside `PluginPanelRenderResult`, and the generator
    // does not pull it in. It is declared here, like any other top-level
    // type, or the artifact would publish a method whose response it does
    // not describe.
    panel_frame: methods::PanelFrame,
    plugin_panel_info: methods::PluginPanelInfo,
    plugin_panel_render_params: methods::PluginPanelRenderParams,
    plugin_panel_render_result: methods::PluginPanelRenderResult,
    plugin_load_error: PluginLoadError,
    plugin_preview: PluginPreview,
    plugin_preview_params: PluginPreviewParams,
    plugin_preview_result: PluginPreviewResult,
    plugin_preview_styled: PluginPreviewStyled,
    plugin_preview_styled_params: PluginPreviewStyledParams,
    plugin_preview_styled_result: PluginPreviewStyledResult,
    plugin_thumbnail: PluginThumbnail,
    plugin_thumbnail_params: PluginThumbnailParams,
    plugin_thumbnail_result: PluginThumbnailResult,
    plugin_run_command_params: PluginRunCommandParams,
    plugin_run_command_result: PluginRunCommandResult,
    plugin_set_approval_params: PluginSetApprovalParams,
    plugin_set_approval_result: PluginSetApprovalResult,
    plugin_set_config_params: PluginSetConfigParams,
    plugin_set_config_result: PluginSetConfigResult,
    plugin_set_enabled_params: PluginSetEnabledParams,
    plugin_set_enabled_result: PluginSetEnabledResult,
    plugin_uninstall_params: PluginUninstallParams,
    plugin_uninstall_result: PluginUninstallResult,
    policy_approval_required: PolicyApprovalRequired,
    policy_decide_params: PolicyDecideParams,
    policy_decide_result: PolicyDecideResult,
    policy_pending_result: PolicyPendingResult,
    ai_organize_plan_params: AiOrganizePlanParams,
    ai_organize_plan_result: AiOrganizePlanResult,
    fs_organize_params: FsOrganizeParams,
    organize_move: OrganizeMove,
    plugin_organize_plan_params: PluginOrganizePlanParams,
    journal_list_params: JournalListParams,
    journal_list_result: JournalListResult,
    journal_row: JournalRow,
    journal_undo_after_params: JournalUndoAfterParams,
    policy_undo_report_params: PolicyUndoReportParams,
    policy_undo_report_result: PolicyUndoReportResult,
    policy_undo_session_params: PolicyUndoSessionParams,
    policy_undo_session_result: PolicyUndoSessionResult,
    request_scope_params: RequestScopeParams,
    request_scope_result: RequestScopeResult,
    rel_path: RelPath,
    resume_policy: ResumePolicy,
    root_overlap: RootOverlap,
    rpc_cancel_params: RpcCancelParams,
    search_hits: SearchHits,
    segment: Segment,
    semantic_hit: SemanticHit,
    server_info: ServerInfo,
    session: Session,
    session_get_result: SessionGetResult,
    session_put_params: SessionPutParams,
    session_put_result: SessionPutResult,
    session_release_result: SessionReleaseResult,
    side: Side,
    span_wire: SpanWire,
    step_reversal: StepReversal,
    symlink_policy: SymlinkPolicy,
    sync_apply_params: SyncApplyParams,
    sync_blocker: SyncBlocker,
    sync_blocker_kind: SyncBlockerKind,
    sync_compare_options: SyncCompareOptions,
    sync_counts: SyncCounts,
    sync_failure: SyncFailure,
    sync_failure_cause: SyncFailureCause,
    sync_mode: SyncMode,
    sync_plan_done: SyncPlanDone,
    sync_plan_params: SyncPlanParams,
    sync_reason: SyncReason,
    sync_report_params: SyncReportParams,
    sync_report_result: SyncReportResult,
    sync_step: SyncStep,
    sync_step_kind: SyncStepKind,
    sync_steps_batch: SyncStepsBatch,
    task_cancel_params: TaskCancelParams,
    task_cancel_result: TaskCancelResult,
    task_id: TaskId,
    task_kind: TaskKind,
    task_list_params: TaskListParams,
    task_list_result: TaskListResult,
    task_move_params: TaskMoveParams,
    task_move_result: TaskMoveResult,
    task_pause_params: TaskPauseParams,
    task_pause_result: TaskPauseResult,
    task_progress: TaskProgress,
    task_state: TaskState,
    undo_blocked: UndoBlocked,
    v_path: VPath,
    verify_policy: VerifyPolicy,
    volume: Volume,
    volume_kind: VolumeKind,
}

#[test]
fn the_protocol_schema_does_not_diverge() {
    let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();
    let json = format!("{}\n", serde_json::to_string_pretty(&schema).unwrap());
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/schema/proto.schema.json");
    if std::env::var_os("NORTE_UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &json).expect("write proto.schema.json");
        return;
    }
    let published = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| {
            panic!(
                "docs/schema/proto.schema.json is missing: regenerate with NORTE_UPDATE_SCHEMA=1"
            )
        })
        .replace("\r\n", "\n");
    assert_eq!(
        published, json,
        "docs/schema/proto.schema.json diverged from the code: regenerate with \
         NORTE_UPDATE_SCHEMA=1 cargo test -p norte-proto --features schema --test schema"
    );
}

/// Completeness guard (rust-review MAJOR): the aggregate [`ProtocolSchema`] is
/// hand-maintained, so a NEW wire type that gains the `schema` derive but is
/// neither added as a field nor referenced by an included type would silently
/// vanish from the artifact while the golden stays green. Every type carrying
/// `#[derive(schemars::JsonSchema)]` (or a hand-written impl) in `src/` MUST
/// appear in the generated `$defs`; scanning the source turns that omission
/// into a red test instead of a stale schema.
#[test]
fn every_type_with_a_schema_is_in_the_artifact() {
    let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();
    let defs = schema
        .get("$defs")
        .and_then(serde_json::Value::as_object)
        .expect("the root schema has $defs");

    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs(&src, &mut files);

    let mut declared: Vec<String> = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read source");
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if line.contains("derive(schemars::JsonSchema)") {
                // Nearest following `pub struct|enum NAME`.
                if let Some(name) = lines[i + 1..].iter().take(8).find_map(|l| pub_type_name(l)) {
                    declared.push(name);
                }
            }
            if let Some(rest) = line
                .trim_start()
                .strip_prefix("impl schemars::JsonSchema for ")
            {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    declared.push(name);
                }
            }
        }
    }
    assert!(
        declared.len() >= 90,
        "the scanner did not find the types with a schema (found {}): did the derive's format change?",
        declared.len()
    );

    let missing: Vec<&String> = declared.iter().filter(|n| !defs.contains_key(*n)).collect();
    assert!(
        missing.is_empty(),
        "types with a `schema` derive missing from the artifact (unreachable from \
         ProtocolSchema — add them as a field): {missing:?}"
    );
}

/// `AttrValue` has a HAND-WRITTEN `JsonSchema` (its serde impls cannot be
/// derived), so nothing but a test keeps it describing what the type really
/// emits. The golden fixture `golden/types/attr_value.json` freezes the tag of
/// every variant, so requiring the two key sets to be equal turns "added a
/// variant, forgot the schema" into a red test.
#[test]
fn the_attr_value_schema_covers_the_golden_s_tags() {
    let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();
    let props = schema
        .pointer("/$defs/AttrValue/properties")
        .and_then(serde_json::Value::as_object)
        .expect("AttrValue has properties in the artifact");
    let from_schema: std::collections::BTreeSet<&str> = props.keys().map(String::as_str).collect();

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/types/attr_value.json");
    let raw = std::fs::read_to_string(&fixture).expect("read attr_value.json");
    let cases: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&raw).expect("valid JSON fixture");
    let from_golden: std::collections::BTreeSet<&str> = cases
        .values()
        .map(|case| {
            let obj = case.as_object().expect("each case is a one-key object");
            assert_eq!(obj.len(), 1, "an AttrValue emits EXACTLY one key");
            obj.keys().next().expect("the key").as_str()
        })
        .collect();

    assert_eq!(
        from_schema, from_golden,
        "$defs/AttrValue's properties and attr_value.json's tags must match"
    );
}

/// (0.36.0) Same mechanism as the test above, for [`RenameCollisionKind`].
/// `golden_types.rs`'s `check_family` does NOT catch this failure: it compares
/// the fixtures against a hand-written list of Rust cases, so a NEW variant
/// with no fixture and no case leaves both sides agreeing and the test green.
/// The artifact, by contrast, is generated from the type, so crossing it
/// against the golden turns "added a verdict, forgot to freeze it" into red.
///
/// `unknown` is left out on purpose: it is the deserialization fallback
/// (`serde(other)`), the core NEVER emits it and so it is not owed a
/// fixture — pinning it would mean freezing a value that does not exist on
/// the wire.
#[test]
fn the_rename_collision_kind_schema_covers_the_golden_s_verdicts() {
    let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();
    let variants = schema
        .pointer("/$defs/RenameCollisionKind/oneOf")
        .and_then(serde_json::Value::as_array)
        .expect("RenameCollisionKind is a oneOf in the artifact");
    let from_schema: std::collections::BTreeSet<&str> = variants
        .iter()
        .filter_map(|v| v.get("const").and_then(serde_json::Value::as_str))
        .filter(|v| *v != "unknown")
        .collect();
    assert!(
        !from_schema.is_empty(),
        "did the enum's shape in the artifact change?"
    );

    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/types/rename_collision.json");
    let raw = std::fs::read_to_string(&fixture).expect("read rename_collision.json");
    let cases: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&raw).expect("valid JSON fixture");
    let from_golden: std::collections::BTreeSet<&str> = cases
        .values()
        .map(|case| {
            case.get("kind")
                .and_then(serde_json::Value::as_str)
                .expect("every collision carries its verdict")
        })
        .collect();

    assert_eq!(
        from_schema, from_golden,
        "every verdict the core can emit needs a fixture in \
         rename_collision.json (and vice versa)"
    );
}

/// (0.41.0, #178) The same mechanism, for the WHOLE ERROR TAXONOMY.
///
/// This is the family that needed it most and the only one that did not have
/// it. A bump that adds a category — two in 0.36.0, one in 0.40.0, one in
/// 0.41.0 — touches three places nothing ties together: the variant, the
/// Rust case in `golden_error`, and the fixture. `check_family` crosses the
/// LAST two, so forgetting both at once leaves both sides agreeing and the
/// test green, with a category the core emits and no golden freezing. The
/// artifact is generated from the TYPE, so crossing it against the fixture
/// closes the triangle.
///
/// [`Error`] carries an INTERNAL tag (`#[serde(tag = "kind")]`), not like the
/// plain-token enums of the two tests below: its `const`s do not live at
/// `oneOf[].const` but at `oneOf[].properties.kind.const`. Hence its own test
/// instead of one more row in the table below.
///
/// `unknown` is left out, like its siblings: it is `serde(other)`'s fallback
/// and the core NEVER emits it.
#[test]
fn the_error_taxonomy_schema_covers_the_golden() {
    let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();
    let variants = schema
        .pointer("/$defs/Error/oneOf")
        .and_then(serde_json::Value::as_array)
        .expect("Error is a oneOf in the artifact");
    let from_schema: std::collections::BTreeSet<&str> = variants
        .iter()
        .filter_map(|v| {
            v.pointer("/properties/kind/const")
                .and_then(serde_json::Value::as_str)
        })
        .filter(|v| *v != "unknown")
        .collect();
    assert!(
        !from_schema.is_empty(),
        "did Error's shape in the artifact change? (internal tag: /properties/kind/const)"
    );

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/types/error.json");
    let raw = std::fs::read_to_string(&fixture).expect("read error.json");
    let cases: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&raw).expect("valid JSON fixture");
    let from_golden: std::collections::BTreeSet<&str> = cases
        .values()
        .map(|case| {
            case.get("kind")
                .and_then(serde_json::Value::as_str)
                .expect("every error in the fixture carries its category")
        })
        .collect();

    assert_eq!(
        from_schema, from_golden,
        "every category the core can emit needs a fixture in error.json (and vice versa)"
    );
}

/// (0.40.0, ADR 0049) The same mechanism as the [`RenameCollisionKind`] test,
/// for the synchronization vocabulary: EVERY token the core can emit needs a
/// fixture, and vice versa.
///
/// This matters more here than in any other family: these enums grow across
/// thirteen more tasks of this same plan, and `check_family` does not catch a
/// new variant with no fixture — it compares the fixtures against a
/// hand-written list of Rust cases, so forgetting both leaves both sides
/// agreeing. The artifact, by contrast, is generated from the type.
///
/// `unknown` is left out in every case: it is `serde(other)`'s fallback, the
/// core NEVER emits it, and freezing it would mean freezing a value that does
/// not exist on the wire.
#[test]
fn the_sync_vocabulary_schema_covers_the_goldens() {
    let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();
    // (type, file, fixture prefix, field). The PREFIX bounds the sweep:
    // `methods.json` is heterogeneous and its `mode` key is also used by
    // `fs.delete`, which has nothing to do with a sync mode.
    for (kind, fixture, prefix, field) in [
        ("SyncStepKind", "sync_step.json", "", "kind"),
        ("StepReversal", "sync_step.json", "", "reversal"),
        ("SyncReason", "sync_step.json", "", "reason"),
        ("SyncBlockerKind", "sync_blocker.json", "", "kind"),
        ("SyncFailureCause", "methods.json", "sync_", "cause"),
        ("SyncMode", "methods.json", "sync_", "mode"),
        // The prefix is `sync_` and not `sync_plan_done` since 0.42.0: the
        // destination's trash also travels in the REPORT (#170), and the
        // sweep has to see both places — if they ever diverged, whichever one
        // fell short of values would break here.
        ("DestTrash", "methods.json", "sync_", "dest_trash"),
        ("OnUnknown", "methods.json", "sync_", "on_unknown"),
        ("RootOverlap", "error.json", "overlapping_roots", "relation"),
        // (0.45.0) The conflict taxonomy was not cross-checked, and that is
        // how `escapes_root` reached the schema with no fixture:
        // `check_family` compares against a hand-written list, so forgetting
        // it in both places at once leaves the test green.
        ("ConflictKind", "error.json", "conflict_", "conflict"),
        // (0.42.0, #152) From the COMPARISON family, and here by the same
        // mechanism: it is the vocabulary a sync plan has to read so as not
        // to write over a file that only pairs through an NFC singleton
        // decomposition.
        ("PairTransform", "compare_row.json", "", "paired_under"),
    ] {
        let variants = schema
            .pointer(&format!("/$defs/{kind}/oneOf"))
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("{kind} is a oneOf in the artifact"));
        let from_schema: std::collections::BTreeSet<&str> = variants
            .iter()
            .filter_map(|v| v.get("const").and_then(serde_json::Value::as_str))
            .filter(|v| *v != "unknown")
            .collect();
        assert!(
            !from_schema.is_empty(),
            "[{kind}] did the enum's shape in the artifact change?"
        );

        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden/types")
            .join(fixture);
        let raw = std::fs::read_to_string(&path).expect("read the fixture");
        let cases: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&raw).expect("valid JSON fixture");
        // The field can be optional (`reversal`, `reason`) and can live
        // NESTED (`cause`, inside the report's list of failures): what is
        // crossed is the set of values the fixtures show anywhere, wherever
        // they live.
        let mut from_golden = std::collections::BTreeSet::new();
        for (name, case) in &cases {
            if name.starts_with(prefix) {
                collect_values(case, field, &mut from_golden);
            }
        }

        assert_eq!(
            from_schema, from_golden,
            "[{kind}] every value the core can emit needs a fixture in \
             {fixture} (and vice versa)"
        );
    }
}

/// (rust-review MINOR 5) The artifact is the only thing a THIRD-PARTY
/// implementer reads: if `Entry.attrs` were an open `object`, it would be
/// telling them that 100 arbitrary keys are legal. The type's constraints
/// have to travel in the schema, and the numbers have to come from the SAME
/// constants the deserializer applies.
///
/// The `pattern` is the ECMA-262 translation of [`is_valid_attr_id`]: one or
/// more `[a-z0-9_-]` segments separated by dots, with at least one dot. `$`
/// with no `m` flag anchors to the end of the string, so it does not admit
/// the trailing `\n` other dialects would let through. The function is the
/// source of truth; this test fails if someone moves one without the other.
#[test]
fn the_entry_attrs_schema_carries_the_type_s_caps() {
    use norte_proto::attrs::{ATTR_ID_MAX, ATTRS_MAX_REQUEST};

    let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();
    let attrs = schema
        .pointer("/$defs/Entry/properties/attrs")
        .expect("Entry.attrs is in the artifact");

    assert_eq!(
        attrs
            .get("maxProperties")
            .and_then(serde_json::Value::as_u64),
        Some(ATTRS_MAX_REQUEST as u64),
        "the map's cap travels in the schema"
    );
    let names = attrs
        .get("propertyNames")
        .expect("keys are restricted, not a plain string");
    assert_eq!(
        names.get("maxLength").and_then(serde_json::Value::as_u64),
        Some(ATTR_ID_MAX as u64)
    );
    assert_eq!(
        names.get("pattern").and_then(serde_json::Value::as_str),
        Some(r"^[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+$"),
        "the pattern is the ECMA-262 translation of is_valid_attr_id"
    );

    // Sample check pattern ⇄ function agreement: what the schema declares
    // legal the validator accepts, and what it declares illegal it rejects.
    for legal in [
        "posix.mode",
        "s3.storage_class",
        "archive.packed-size",
        "a.b",
    ] {
        assert!(norte_proto::attrs::is_valid_attr_id(legal));
    }
    for illegal in [
        "mode",
        "MODE",
        "../etc/passwd",
        "posix.",
        ".mode",
        "a..b",
        "",
        // A segment that does not start with a letter (0.30.0): argv-shaped
        // and float-shaped, which downstream are read as something else.
        "-x.y",
        "0.0",
        "9-9.9-9",
        "__.__",
    ] {
        assert!(!norte_proto::attrs::is_valid_attr_id(illegal));
    }
}

/// (0.30.0, ADR 0039) Same criterion as the previous test, for the method's
/// three fields: the artifact is the only thing a THIRD-PARTY implementer
/// reads, and an open `array` would tell them 100 arbitrary ids are legal.
/// The caps come from the SAME constants the code applies.
///
/// The catalog carries `maxItems`; its ELEMENT's shape travels in
/// `$defs/AttrInfo`, which restricts `id` (pattern + length) and `label`
/// (length) — the SAME rules `sanitize_catalog` applies on decode. The two
/// REQUESTS restrict the item on the field itself, because there a malformed
/// id is `-32602` and the schema has to say so.
#[test]
fn the_method_field_schemas_carry_the_type_s_caps() {
    use norte_proto::attrs::{
        ATTR_ID_MAX, ATTR_LABEL_MAX, ATTRS_MAX_ADVERTISED, ATTRS_MAX_REQUEST,
    };

    let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();

    // The advertised descriptor: id with shape and cap, label with a cap.
    let id = schema
        .pointer("/$defs/AttrInfo/properties/id")
        .expect("AttrInfo.id is in the artifact");
    assert_eq!(
        id.get("maxLength").and_then(serde_json::Value::as_u64),
        Some(ATTR_ID_MAX as u64),
        "the id's cap travels in the schema"
    );
    assert_eq!(
        id.get("pattern").and_then(serde_json::Value::as_str),
        Some(r"^[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+$"),
        "the pattern is the SAME as Entry.attrs's"
    );
    assert_eq!(
        schema
            .pointer("/$defs/AttrInfo/properties/label/maxLength")
            .and_then(serde_json::Value::as_u64),
        Some(ATTR_LABEL_MAX as u64),
        "the label's cap travels in the schema (and `sanitize_catalog` trims it)"
    );

    let catalog = schema
        .pointer("/$defs/FsCapabilitiesResult/properties/attrs")
        .expect("FsCapabilitiesResult.attrs is in the artifact");
    assert_eq!(
        catalog.get("maxItems").and_then(serde_json::Value::as_u64),
        Some(ATTRS_MAX_ADVERTISED as u64),
        "the catalog's cap travels in the schema"
    );
    assert_eq!(
        catalog
            .pointer("/items/$ref")
            .and_then(serde_json::Value::as_str),
        Some("#/$defs/AttrInfo"),
        "the element is an AttrInfo, not a free-form object"
    );

    for kind in ["FsListParams", "FsStatParams"] {
        let requested = schema
            .pointer(&format!("/$defs/{kind}/properties/attrs"))
            .unwrap_or_else(|| panic!("{kind}.attrs is in the artifact"));
        assert_eq!(
            requested
                .get("maxItems")
                .and_then(serde_json::Value::as_u64),
            Some(ATTRS_MAX_REQUEST as u64),
            "[{kind}] the requested-ids cap travels in the schema"
        );
        let item = requested
            .get("items")
            .unwrap_or_else(|| panic!("[{kind}] ids are restricted, not a plain string"));
        assert_eq!(
            item.get("maxLength").and_then(serde_json::Value::as_u64),
            Some(ATTR_ID_MAX as u64),
            "[{kind}] the id's maximum length"
        );
        assert_eq!(
            item.get("pattern").and_then(serde_json::Value::as_str),
            Some(r"^[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+$"),
            "[{kind}] the pattern is the SAME as Entry.attrs's \
             (ECMA-262 translation of is_valid_attr_id)"
        );
    }
}

/// (0.36.0) Same criterion as the `attrs` fields test: the artifact is the
/// only thing a THIRD-PARTY implementer reads, and a ceiling-less `array`
/// would tell them a batch of a million renames is legal. It matters more
/// here than in `attrs`, because going over is NOT trimmed — it is
/// `-32602`, the whole request — so a client that does not know the number
/// sends something the daemon throws away.
///
/// The numbers come from the SAME constants the code applies, and the hash's
/// pattern comes from [`PlanHash`]'s `JsonSchema`, which in turn builds it
/// from `PLAN_HASH_LEN`: a single source for the validator and the contract.
#[test]
fn the_rename_batch_schema_carries_the_type_s_caps() {
    use norte_proto::methods::{FS_RENAME_BATCH_MAX_PAIRS, PLAN_HASH_LEN};

    let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();

    for kind in ["FsRenameBatchPlanParams", "FsRenameBatchParams"] {
        let pairs = schema
            .pointer(&format!("/$defs/{kind}/properties/pairs"))
            .unwrap_or_else(|| panic!("{kind}.pairs is in the artifact"));
        assert_eq!(
            pairs.get("maxItems").and_then(serde_json::Value::as_u64),
            Some(FS_RENAME_BATCH_MAX_PAIRS as u64),
            "[{kind}] the pair cap travels in the schema"
        );
        assert_eq!(
            pairs
                .pointer("/items/$ref")
                .and_then(serde_json::Value::as_str),
            Some("#/$defs/RenamePair"),
            "[{kind}] the element is a RenamePair, not a free-form object"
        );
    }

    // The hash carries its shape in the TYPE, so both fields are a `$ref`
    // and the constraints live in exactly one place.
    let hash = schema
        .pointer("/$defs/PlanHash")
        .expect("PlanHash is in the artifact");
    assert_eq!(
        hash.get("pattern").and_then(serde_json::Value::as_str),
        Some(format!("^[0-9a-f]{{{PLAN_HASH_LEN}}}$").as_str()),
        "the pattern is the ECMA-262 translation of PlanHash::parse"
    );
    for cap in ["minLength", "maxLength"] {
        assert_eq!(
            hash.get(cap).and_then(serde_json::Value::as_u64),
            Some(PLAN_HASH_LEN as u64),
            "[{cap}] the EXACT length travels in the schema"
        );
    }
    for kind in ["FsRenameBatchPlanResult", "FsRenameBatchParams"] {
        assert_eq!(
            schema
                .pointer(&format!("/$defs/{kind}/properties/plan_hash/$ref"))
                .and_then(serde_json::Value::as_str),
            Some("#/$defs/PlanHash"),
            "[{kind}] plan_hash is the type with a pattern, not a plain string"
        );
    }
}

/// Collects, in depth, every string-typed value hanging off the `field` key.
/// Fixtures nest (a report carries its failures in a list), so a crosscheck
/// that only looked at the first level would leave whole enums unwatched.
fn collect_values<'a>(
    value: &'a serde_json::Value,
    field: &str,
    out: &mut std::collections::BTreeSet<&'a str>,
) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if k == field
                    && let Some(s) = v.as_str()
                {
                    out.insert(s);
                }
                collect_values(v, field, out);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                collect_values(v, field, out);
            }
        }
        _ => {}
    }
}

/// Collects `.rs` files under `dir` (includes `src/wire/`).
fn collect_rs(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Name in `pub struct NAME` / `pub enum NAME`, if the line is one.
fn pub_type_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = trimmed
        .strip_prefix("pub struct ")
        .or_else(|| trimmed.strip_prefix("pub enum "))?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}
