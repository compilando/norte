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
    // `PanelEvent` tampoco es alcanzable, y por lo mismo que `PanelFrame`:
    // va APLANADO dentro de los params, y el generador no arrastra lo que
    // `flatten` esconde. Sin esta línea el artefacto publicaría un método
    // cuya petición no describe del todo.
    panel_event: methods::PanelEvent,
    // Sin `panel_span`: un tramo de panel es un `SpanWire`, el mismo que el
    // de una preview estilada, y ese ya está en el artefacto.
    //
    // `PanelFrame` no es alcanzable desde el resultado: viaja con
    // `#[serde(flatten)]` dentro de `PluginPanelRenderResult`, y el generador
    // no lo arrastra. Se declara aquí, como cualquier otro tipo de primer
    // nivel, o el artefacto publicaría un método cuya respuesta no describe.
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
    task_progress: TaskProgress,
    task_state: TaskState,
    undo_blocked: UndoBlocked,
    v_path: VPath,
    verify_policy: VerifyPolicy,
    volume: Volume,
    volume_kind: VolumeKind,
}

#[test]
fn el_schema_del_protocolo_no_diverge() {
    let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();
    let json = format!("{}\n", serde_json::to_string_pretty(&schema).unwrap());
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/schema/proto.schema.json");
    if std::env::var_os("NORTE_UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &json).expect("escribir proto.schema.json");
        return;
    }
    let publicado = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| {
            panic!("falta docs/schema/proto.schema.json: regenera con NORTE_UPDATE_SCHEMA=1")
        })
        .replace("\r\n", "\n");
    assert_eq!(
        publicado, json,
        "docs/schema/proto.schema.json divergió del código: regenera con \
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
fn todo_tipo_con_schema_esta_en_el_artefacto() {
    let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();
    let defs = schema
        .get("$defs")
        .and_then(serde_json::Value::as_object)
        .expect("el schema raíz tiene $defs");

    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs(&src, &mut files);

    let mut declared: Vec<String> = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("leer fuente");
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
        "el escáner no encontró los tipos con schema (halló {}): ¿cambió el formato del derive?",
        declared.len()
    );

    let missing: Vec<&String> = declared.iter().filter(|n| !defs.contains_key(*n)).collect();
    assert!(
        missing.is_empty(),
        "tipos con derive `schema` ausentes del artefacto (no alcanzables desde \
         ProtocolSchema — añádelos como campo): {missing:?}"
    );
}

/// `AttrValue` has a HAND-WRITTEN `JsonSchema` (its serde impls cannot be
/// derived), so nothing but a test keeps it describing what the type really
/// emits. The golden fixture `golden/types/attr_value.json` freezes the tag of
/// every variant, so requiring the two key sets to be equal turns "added a
/// variant, forgot the schema" into a red test.
#[test]
fn el_schema_de_attr_value_cubre_las_etiquetas_de_la_golden() {
    let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();
    let props = schema
        .pointer("/$defs/AttrValue/properties")
        .and_then(serde_json::Value::as_object)
        .expect("AttrValue tiene properties en el artefacto");
    let del_schema: std::collections::BTreeSet<&str> = props.keys().map(String::as_str).collect();

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/types/attr_value.json");
    let raw = std::fs::read_to_string(&fixture).expect("leer attr_value.json");
    let casos: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&raw).expect("fixture JSON válida");
    let de_la_golden: std::collections::BTreeSet<&str> = casos
        .values()
        .map(|caso| {
            let obj = caso
                .as_object()
                .expect("cada caso es un objeto de una clave");
            assert_eq!(obj.len(), 1, "un AttrValue emite EXACTAMENTE una clave");
            obj.keys().next().expect("la clave").as_str()
        })
        .collect();

    assert_eq!(
        del_schema, de_la_golden,
        "las properties de $defs/AttrValue y las etiquetas de attr_value.json deben coincidir"
    );
}

/// (0.36.0) Mismo mecanismo que el test de arriba, para
/// [`RenameCollisionKind`]. El `check_family` de `golden_types.rs` NO cubre
/// este fallo: compara las fixtures contra una lista de casos Rust escrita a
/// mano, así que una variante NUEVA sin fixture y sin caso deja los dos lados
/// de acuerdo y el test verde. El artefacto, en cambio, se genera del tipo, así
/// que cruzarlo contra la golden convierte «añadí un veredicto, olvidé
/// congelarlo» en rojo.
///
/// `unknown` queda fuera a propósito: es el fallback de deserialización
/// (`serde(other)`), el core JAMÁS lo emite y por eso no le corresponde
/// fixture — pinearlo obligaría a congelar un valor que no existe en el wire.
#[test]
fn el_schema_de_rename_collision_kind_cubre_los_veredictos_de_la_golden() {
    let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();
    let variantes = schema
        .pointer("/$defs/RenameCollisionKind/oneOf")
        .and_then(serde_json::Value::as_array)
        .expect("RenameCollisionKind es un oneOf en el artefacto");
    let del_schema: std::collections::BTreeSet<&str> = variantes
        .iter()
        .filter_map(|v| v.get("const").and_then(serde_json::Value::as_str))
        .filter(|v| *v != "unknown")
        .collect();
    assert!(
        !del_schema.is_empty(),
        "¿cambió la forma del enum en el artefacto?"
    );

    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/types/rename_collision.json");
    let raw = std::fs::read_to_string(&fixture).expect("leer rename_collision.json");
    let casos: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&raw).expect("fixture JSON válida");
    let de_la_golden: std::collections::BTreeSet<&str> = casos
        .values()
        .map(|caso| {
            caso.get("kind")
                .and_then(serde_json::Value::as_str)
                .expect("cada colisión lleva su veredicto")
        })
        .collect();

    assert_eq!(
        del_schema, de_la_golden,
        "todo veredicto que el core puede emitir necesita fixture en \
         rename_collision.json (y al revés)"
    );
}

/// (0.41.0, #178) El mismo mecanismo, para LA TAXONOMÍA DE ERRORES entera.
///
/// Es la familia a la que más le hacía falta y la única que no lo tenía. Un
/// bump que añade categoría —0.36.0 dos, 0.40.0 una, 0.41.0 una— toca tres
/// sitios que nada ata entre sí: la variante, el caso Rust de `golden_error` y
/// la fixture. `check_family` cruza los dos ÚLTIMOS, así que olvidar los dos a
/// la vez deja los dos lados de acuerdo y el test verde, con una categoría que
/// el core emite y ninguna golden congela. El artefacto se genera del TIPO, así
/// que cruzarlo contra la fixture cierra el triángulo.
///
/// [`Error`] va con tag INTERNO (`#[serde(tag = "kind")]`), no como los enums
/// de token suelto de los dos tests de abajo: sus `const` no viven en
/// `oneOf[].const` sino en `oneOf[].properties.kind.const`. De ahí que sea un
/// test propio y no una fila más de la tabla.
///
/// `unknown` queda fuera, como en sus hermanos: es el fallback de
/// `serde(other)` y el core JAMÁS lo emite.
#[test]
fn el_schema_de_la_taxonomia_de_errores_cubre_la_golden() {
    let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();
    let variantes = schema
        .pointer("/$defs/Error/oneOf")
        .and_then(serde_json::Value::as_array)
        .expect("Error es un oneOf en el artefacto");
    let del_schema: std::collections::BTreeSet<&str> = variantes
        .iter()
        .filter_map(|v| {
            v.pointer("/properties/kind/const")
                .and_then(serde_json::Value::as_str)
        })
        .filter(|v| *v != "unknown")
        .collect();
    assert!(
        !del_schema.is_empty(),
        "¿cambió la forma de Error en el artefacto? (tag interno: /properties/kind/const)"
    );

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/types/error.json");
    let raw = std::fs::read_to_string(&fixture).expect("leer error.json");
    let casos: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&raw).expect("fixture JSON válida");
    let de_la_golden: std::collections::BTreeSet<&str> = casos
        .values()
        .map(|caso| {
            caso.get("kind")
                .and_then(serde_json::Value::as_str)
                .expect("cada error de la fixture lleva su categoría")
        })
        .collect();

    assert_eq!(
        del_schema, de_la_golden,
        "toda categoría que el core puede emitir necesita fixture en error.json (y al revés)"
    );
}

/// (0.40.0, ADR 0049) El mismo mecanismo que el test de
/// [`RenameCollisionKind`], para el vocabulario de la sincronización: TODO
/// token que el core puede emitir necesita fixture, y al revés.
///
/// Importa más aquí que en ninguna otra familia: estos enums crecen a lo largo
/// de trece tareas más de este mismo plan, y `check_family` no caza la
/// variante nueva sin fixture —compara las fixtures contra una lista de casos
/// Rust escrita a mano, así que olvidar las dos deja los dos lados de acuerdo—.
/// El artefacto, en cambio, se genera del tipo.
///
/// `unknown` queda fuera en todos los casos: es el fallback de `serde(other)`,
/// el core JAMÁS lo emite y congelarlo sería congelar un valor que no existe en
/// el wire.
#[test]
fn el_schema_del_vocabulario_de_sync_cubre_las_goldens() {
    let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();
    // (tipo, fichero, prefijo de fixture, clave). El PREFIJO acota el barrido:
    // `methods.json` es heterogéneo y su clave `mode` también la usa
    // `fs.delete`, que no tiene nada que ver con un modo de sincronización.
    for (tipo, fixture, prefijo, campo) in [
        ("SyncStepKind", "sync_step.json", "", "kind"),
        ("StepReversal", "sync_step.json", "", "reversal"),
        ("SyncReason", "sync_step.json", "", "reason"),
        ("SyncBlockerKind", "sync_blocker.json", "", "kind"),
        ("SyncFailureCause", "methods.json", "sync_", "cause"),
        ("SyncMode", "methods.json", "sync_", "mode"),
        // El prefijo es `sync_` y no `sync_plan_done` desde 0.42.0: la papelera
        // del destino viaja también en el INFORME (#170), y el barrido tiene
        // que ver los dos sitios — si un día divergieran, el que se quedara
        // corto de valores rompería aquí.
        ("DestTrash", "methods.json", "sync_", "dest_trash"),
        ("OnUnknown", "methods.json", "sync_", "on_unknown"),
        ("RootOverlap", "error.json", "overlapping_roots", "relation"),
        // (0.45.0) La taxonomía de conflictos no estaba cruzada, y por eso
        // `escapes_root` pudo llegar al schema sin fixture: `check_family`
        // compara contra una lista escrita a mano, así que olvidarse en los dos
        // sitios a la vez deja el test verde.
        ("ConflictKind", "error.json", "conflict_", "conflict"),
        // (0.42.0, #152) De la familia de COMPARACIÓN, y aquí por el mismo
        // mecanismo: es el vocabulario que un plan de sincronización tiene que
        // leer para no escribir sobre un fichero que solo empareja por una
        // descomposición singleton de NFC.
        ("PairTransform", "compare_row.json", "", "paired_under"),
    ] {
        let variantes = schema
            .pointer(&format!("/$defs/{tipo}/oneOf"))
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("{tipo} es un oneOf en el artefacto"));
        let del_schema: std::collections::BTreeSet<&str> = variantes
            .iter()
            .filter_map(|v| v.get("const").and_then(serde_json::Value::as_str))
            .filter(|v| *v != "unknown")
            .collect();
        assert!(
            !del_schema.is_empty(),
            "[{tipo}] ¿cambió la forma del enum en el artefacto?"
        );

        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden/types")
            .join(fixture);
        let raw = std::fs::read_to_string(&path).expect("leer la fixture");
        let casos: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&raw).expect("fixture JSON válida");
        // El campo puede ser opcional (`reversal`, `reason`) y puede vivir
        // ANIDADO (`cause`, dentro de la lista de fallos del informe): lo que se
        // cruza es el conjunto de valores que las fixtures llegan a enseñar,
        // esté donde esté.
        let mut de_la_golden = std::collections::BTreeSet::new();
        for (nombre, caso) in &casos {
            if nombre.starts_with(prefijo) {
                recoge_valores(caso, campo, &mut de_la_golden);
            }
        }

        assert_eq!(
            del_schema, de_la_golden,
            "[{tipo}] todo valor que el core puede emitir necesita fixture en \
             {fixture} (y al revés)"
        );
    }
}

/// (rust-review MINOR 5) El artefacto es lo único que un implementador de
/// TERCEROS lee: si `Entry.attrs` fuese un `object` abierto, le estaría
/// diciendo que 100 claves arbitrarias son legales. Las restricciones del tipo
/// tienen que viajar en el schema, y los números tienen que venir de las
/// MISMAS constantes que aplica el deserializador.
///
/// El `pattern` es la traducción ECMA-262 de [`is_valid_attr_id`]: uno o más
/// segmentos `[a-z0-9_-]` separados por puntos, con al menos un punto. `$` sin
/// flag `m` ancla al final de la cadena, así que no admite el `\n` final que
/// sí colaría en otros dialectos. La función es la verdad; este test falla si
/// alguien mueve una sin la otra.
#[test]
fn el_schema_de_entry_attrs_lleva_los_topes_del_tipo() {
    use norte_proto::attrs::{ATTR_ID_MAX, ATTRS_MAX_REQUEST};

    let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();
    let attrs = schema
        .pointer("/$defs/Entry/properties/attrs")
        .expect("Entry.attrs está en el artefacto");

    assert_eq!(
        attrs
            .get("maxProperties")
            .and_then(serde_json::Value::as_u64),
        Some(ATTRS_MAX_REQUEST as u64),
        "el tope del mapa viaja en el schema"
    );
    let nombres = attrs
        .get("propertyNames")
        .expect("las claves están restringidas, no son un string cualquiera");
    assert_eq!(
        nombres.get("maxLength").and_then(serde_json::Value::as_u64),
        Some(ATTR_ID_MAX as u64)
    );
    assert_eq!(
        nombres.get("pattern").and_then(serde_json::Value::as_str),
        Some(r"^[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+$"),
        "el patrón es la traducción ECMA-262 de is_valid_attr_id"
    );

    // Muestreo de acuerdo patrón ⇄ función: lo que el schema declara legal lo
    // acepta el validador, y lo que declara ilegal lo rechaza.
    for legal in [
        "posix.mode",
        "s3.storage_class",
        "archive.packed-size",
        "a.b",
    ] {
        assert!(norte_proto::attrs::is_valid_attr_id(legal));
    }
    for ilegal in [
        "mode",
        "MODE",
        "../etc/passwd",
        "posix.",
        ".mode",
        "a..b",
        "",
        // Segmento que no empieza por letra (0.30.0): forma de argv y forma
        // de float, que aguas abajo se leen como otra cosa.
        "-x.y",
        "0.0",
        "9-9.9-9",
        "__.__",
    ] {
        assert!(!norte_proto::attrs::is_valid_attr_id(ilegal));
    }
}

/// (0.30.0, ADR 0039) Mismo criterio que el test anterior, para los tres campos
/// de método: el artefacto es lo único que lee un implementador de TERCEROS, y
/// un `array` abierto le diría que 100 ids arbitrarios son legales. Los topes
/// vienen de las MISMAS constantes que aplica el código.
///
/// El catálogo lleva `maxItems`; la forma de su ELEMENTO viaja en
/// `$defs/AttrInfo`, que restringe `id` (patrón + longitud) y `label`
/// (longitud) — las MISMAS reglas que aplica `sanitize_catalog` al decodificar.
/// Las dos PETICIONES restringen el ítem en el propio campo, porque ahí un id
/// mal formado es `-32602` y el schema tiene que decirlo.
#[test]
fn el_schema_de_los_campos_de_metodo_lleva_los_topes_del_tipo() {
    use norte_proto::attrs::{
        ATTR_ID_MAX, ATTR_LABEL_MAX, ATTRS_MAX_ADVERTISED, ATTRS_MAX_REQUEST,
    };

    let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();

    // El descriptor anunciado: id con forma y tope, label con tope.
    let id = schema
        .pointer("/$defs/AttrInfo/properties/id")
        .expect("AttrInfo.id está en el artefacto");
    assert_eq!(
        id.get("maxLength").and_then(serde_json::Value::as_u64),
        Some(ATTR_ID_MAX as u64),
        "el tope del id viaja en el schema"
    );
    assert_eq!(
        id.get("pattern").and_then(serde_json::Value::as_str),
        Some(r"^[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+$"),
        "el patrón es el MISMO que el de Entry.attrs"
    );
    assert_eq!(
        schema
            .pointer("/$defs/AttrInfo/properties/label/maxLength")
            .and_then(serde_json::Value::as_u64),
        Some(ATTR_LABEL_MAX as u64),
        "el tope del label viaja en el schema (y `sanitize_catalog` lo recorta)"
    );

    let catalogo = schema
        .pointer("/$defs/FsCapabilitiesResult/properties/attrs")
        .expect("FsCapabilitiesResult.attrs está en el artefacto");
    assert_eq!(
        catalogo.get("maxItems").and_then(serde_json::Value::as_u64),
        Some(ATTRS_MAX_ADVERTISED as u64),
        "el tope del catálogo viaja en el schema"
    );
    assert_eq!(
        catalogo
            .pointer("/items/$ref")
            .and_then(serde_json::Value::as_str),
        Some("#/$defs/AttrInfo"),
        "el elemento es un AttrInfo, no un objeto libre"
    );

    for tipo in ["FsListParams", "FsStatParams"] {
        let pedido = schema
            .pointer(&format!("/$defs/{tipo}/properties/attrs"))
            .unwrap_or_else(|| panic!("{tipo}.attrs está en el artefacto"));
        assert_eq!(
            pedido.get("maxItems").and_then(serde_json::Value::as_u64),
            Some(ATTRS_MAX_REQUEST as u64),
            "[{tipo}] el tope de ids pedidos viaja en el schema"
        );
        let item = pedido.get("items").unwrap_or_else(|| {
            panic!("[{tipo}] los ids están restringidos, no son un string cualquiera")
        });
        assert_eq!(
            item.get("maxLength").and_then(serde_json::Value::as_u64),
            Some(ATTR_ID_MAX as u64),
            "[{tipo}] longitud máxima del id"
        );
        assert_eq!(
            item.get("pattern").and_then(serde_json::Value::as_str),
            Some(r"^[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+$"),
            "[{tipo}] el patrón es el MISMO que el de Entry.attrs \
             (traducción ECMA-262 de is_valid_attr_id)"
        );
    }
}

/// (0.36.0) Mismo criterio que el test de los campos de `attrs`: el artefacto
/// es lo único que lee un implementador de TERCEROS, y un `array` sin techo le
/// diría que un lote de un millón de renames es legal. Aquí importa más que en
/// `attrs`, porque pasarse NO se recorta — es `-32602`, la petición entera —,
/// así que un cliente que no conozca el número manda algo que el daemon tira.
///
/// Los números salen de las MISMAS constantes que aplica el código, y el patrón
/// del hash sale del `JsonSchema` de [`PlanHash`], que a su vez lo construye
/// desde `PLAN_HASH_LEN`: una sola fuente para el validador y para el contrato.
#[test]
fn el_schema_del_batch_de_renames_lleva_los_topes_del_tipo() {
    use norte_proto::methods::{FS_RENAME_BATCH_MAX_PAIRS, PLAN_HASH_LEN};

    let schema = serde_json::to_value(schemars::schema_for!(ProtocolSchema)).unwrap();

    for tipo in ["FsRenameBatchPlanParams", "FsRenameBatchParams"] {
        let pairs = schema
            .pointer(&format!("/$defs/{tipo}/properties/pairs"))
            .unwrap_or_else(|| panic!("{tipo}.pairs está en el artefacto"));
        assert_eq!(
            pairs.get("maxItems").and_then(serde_json::Value::as_u64),
            Some(FS_RENAME_BATCH_MAX_PAIRS as u64),
            "[{tipo}] el tope de parejas viaja en el schema"
        );
        assert_eq!(
            pairs
                .pointer("/items/$ref")
                .and_then(serde_json::Value::as_str),
            Some("#/$defs/RenamePair"),
            "[{tipo}] el elemento es un RenamePair, no un objeto libre"
        );
    }

    // El hash lleva su forma en el TIPO, así que los dos campos son un `$ref`
    // y las restricciones viven una sola vez.
    let hash = schema
        .pointer("/$defs/PlanHash")
        .expect("PlanHash está en el artefacto");
    assert_eq!(
        hash.get("pattern").and_then(serde_json::Value::as_str),
        Some(format!("^[0-9a-f]{{{PLAN_HASH_LEN}}}$").as_str()),
        "el patrón es la traducción ECMA-262 de PlanHash::parse"
    );
    for tope in ["minLength", "maxLength"] {
        assert_eq!(
            hash.get(tope).and_then(serde_json::Value::as_u64),
            Some(PLAN_HASH_LEN as u64),
            "[{tope}] la longitud EXACTA viaja en el schema"
        );
    }
    for tipo in ["FsRenameBatchPlanResult", "FsRenameBatchParams"] {
        assert_eq!(
            schema
                .pointer(&format!("/$defs/{tipo}/properties/plan_hash/$ref"))
                .and_then(serde_json::Value::as_str),
            Some("#/$defs/PlanHash"),
            "[{tipo}] plan_hash es el tipo con patrón, no un string cualquiera"
        );
    }
}

/// Recoge, en profundidad, todo valor de tipo string que cuelgue de la clave
/// `campo`. Las fixtures anidan (un informe lleva sus fallos en una lista), así
/// que un cruce que solo mirase el primer nivel dejaría enums enteros sin
/// vigilar.
fn recoge_valores<'a>(
    valor: &'a serde_json::Value,
    campo: &str,
    out: &mut std::collections::BTreeSet<&'a str>,
) {
    match valor {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if k == campo
                    && let Some(s) = v.as_str()
                {
                    out.insert(s);
                }
                recoge_valores(v, campo, out);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                recoge_valores(v, campo, out);
            }
        }
        _ => {}
    }
}

/// Recoge `.rs` bajo `dir` (incluye `src/wire/`).
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

/// Nombre en `pub struct NAME` / `pub enum NAME`, si la línea lo es.
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
