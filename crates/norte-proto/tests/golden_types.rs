//! Golden tests de los tipos del protocolo (spec §12): cada fixture JSON es
//! el wire format congelado. Match estructural exacto (`serde_json::Value`)
//! bidireccional — el orden de claves y el formato de whitespace NO son parte
//! del contrato JSON-RPC; nombres, tipos y valores sí. Romper uno de estos
//! tests = cambio de wire format = bump de versión + revisión doble.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::path::Path;

use norte_proto::methods::{
    ClientInfo, DaemonShutdownParams, DaemonShutdownResult, FsCapabilitiesParams,
    FsCapabilitiesResult, FsCopyParams, FsDeleteParams, FsListParams, FsListResult, FsMoveParams,
    FsReadParams, FsReadResult, FsSearchParams, FsStatParams, FsStatResult, FsTaskResult,
    InitializeParams, InitializeResult, MatchInfo, SearchHits, ServerInfo, TaskCancelParams,
    TaskCancelResult, TaskListParams, TaskListResult,
};
use norte_proto::{
    AttrCatalog, AttrHint, AttrInfo, AttrType, AttrValue, ByteRange, Capabilities, CapabilityFlags,
    CollisionPolicy, ConflictKind, Entry, EntryKind, Error, ResumePolicy, SymlinkPolicy, TaskId,
    TaskKind, TaskProgress, TaskState, VPath, VerifyPolicy,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn vpath(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido de fixture")
}

/// Un [`PlanHash`] desde su forma hex (0.36.0). Las fixtures usan hashes
/// SINTÉTICOS: un sha256 real de algo dejaría pasar un hasher que no alimentara
/// nada. El tipo valida igual, que es de lo que se trata.
fn plan_hash(hex: &str) -> norte_proto::methods::PlanHash {
    norte_proto::methods::PlanHash::parse(hex).expect("plan hash de fixture")
}

/// Un nombre BASE desde sus bytes crudos (0.36.0): las fixtures del batch de
/// renames se escriben en bytes, no en la forma percent-encoded — que es
/// justamente lo que el golden tiene que demostrar.
fn seg(b: &[u8]) -> norte_proto::Segment {
    norte_proto::Segment::new(b.to_vec()).expect("segment")
}

fn load(name: &str) -> BTreeMap<String, Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/types")
        .join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("no se pudo leer {}: {e}", path.display()));
    serde_json::from_str(&raw).expect("fixture JSON válida")
}

/// Chequea una familia completa: cobertura 1:1 entre fixture y casos Rust,
/// y match exacto en ambas direcciones para cada caso.
fn check_family<T>(file: &str, cases: &[(&str, T)])
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let fixtures = load(file);
    let fixture_names: Vec<&str> = fixtures.keys().map(String::as_str).collect();
    let mut case_names: Vec<&str> = cases.iter().map(|(n, _)| *n).collect();
    case_names.sort_unstable();
    assert_eq!(
        fixture_names, case_names,
        "[{file}] los casos Rust y las fixtures deben cubrirse 1:1"
    );

    for (name, value) in cases {
        let expected = &fixtures[*name];
        let serialized = serde_json::to_value(value).expect("serializable");
        assert_eq!(&serialized, expected, "[{file}/{name}] serialize");
        let back: T = serde_json::from_value(expected.clone())
            .unwrap_or_else(|e| panic!("[{file}/{name}] deserialize: {e}"));
        assert_eq!(&back, value, "[{file}/{name}] deserialize == construido");
    }
}

/// Chequea un caso suelto contra su entrada de fixture (familias heterogéneas).
fn check_one<T>(fixtures: &BTreeMap<String, Value>, name: &str, value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let expected = fixtures
        .get(name)
        .unwrap_or_else(|| panic!("[methods.json] falta la fixture {name}"));
    assert_eq!(
        &serde_json::to_value(value).expect("serializable"),
        expected,
        "[methods/{name}] serialize"
    );
    let back: T = serde_json::from_value(expected.clone())
        .unwrap_or_else(|e| panic!("[methods/{name}] deserialize: {e}"));
    assert_eq!(&back, value, "[methods/{name}] deserialize == construido");
}

#[test]
fn golden_entry() {
    check_family(
        "entry.json",
        &[
            (
                "file_full",
                Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: vpath("file:///home/user/doc.txt"),
                    kind: EntryKind::File,
                    size: Some(1234),
                    mtime_ms: Some(1_720_000_000_000),
                },
            ),
            (
                "dir_no_meta",
                Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: vpath("file:///home/user"),
                    kind: EntryKind::Dir,
                    size: None,
                    mtime_ms: None,
                },
            ),
            (
                "symlink",
                Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: vpath("file:///ln"),
                    kind: EntryKind::Symlink,
                    size: None,
                    mtime_ms: None,
                },
            ),
            (
                "other_pre_epoch",
                Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: vpath("file:///dev-thing"),
                    kind: EntryKind::Other,
                    size: None,
                    mtime_ms: Some(-86_400_000),
                },
            ),
            (
                "hostile_name",
                Entry {
                    attrs: std::collections::BTreeMap::new(),
                    path: vpath("file:///informe%FF%FE.dat"),
                    kind: EntryKind::File,
                    size: Some(0),
                    mtime_ms: None,
                },
            ),
            (
                "con_attrs",
                Entry {
                    path: vpath("file:///home/user/doc.txt"),
                    kind: EntryKind::File,
                    size: Some(1234),
                    mtime_ms: Some(1_720_000_000_000),
                    attrs: BTreeMap::from([
                        ("posix.mode".to_owned(), AttrValue::Uint(33188)),
                        ("posix.uid".to_owned(), AttrValue::Uint(1000)),
                        ("sftp.owner".to_owned(), AttrValue::Bytes(vec![0xFF, 0xFE])),
                        (
                            "s3.storage_class".to_owned(),
                            AttrValue::Text("STANDARD_IA".to_owned()),
                        ),
                    ]),
                },
            ),
            (
                // Un id que PARECE hostil (larguísimo, con guiones) pero es
                // LEGAL: exactamente `ATTR_ID_MAX` bytes, así que sobrevive al
                // filtrado de decodificación y el match bidireccional se
                // mantiene exacto.
                "attr_id_en_el_tope",
                Entry {
                    path: vpath("file:///home/user/objeto.bin"),
                    kind: EntryKind::File,
                    size: Some(7),
                    mtime_ms: None,
                    attrs: BTreeMap::from([(
                        "s3.x-amz-meta-una_clave_de_usuario_larguisima_pero_legal_64bytes"
                            .to_owned(),
                        AttrValue::Text("sí, 64 bytes exactos".to_owned()),
                    )]),
                },
            ),
            (
                "attrs_vacios_se_omiten",
                Entry {
                    path: vpath("file:///home/user/otro.txt"),
                    kind: EntryKind::File,
                    size: None,
                    mtime_ms: None,
                    attrs: BTreeMap::new(),
                },
            ),
        ],
    );
}

#[test]
fn golden_capabilities() {
    check_family(
        "capabilities.json",
        &[
            (
                "archive_read_only",
                Capabilities {
                    flags: CapabilityFlags::CASE_SENSITIVE
                        | CapabilityFlags::CASE_PRESERVING
                        | CapabilityFlags::READ_ONLY,
                    max_path: None,
                },
            ),
            (
                "local_typical",
                Capabilities {
                    flags: CapabilityFlags::RENAME_ATOMIC
                        | CapabilityFlags::SYMLINKS
                        | CapabilityFlags::CASE_SENSITIVE
                        | CapabilityFlags::CASE_PRESERVING,
                    max_path: Some(4096),
                },
            ),
            (
                "server_copy_only",
                Capabilities {
                    flags: CapabilityFlags::SERVER_COPY,
                    max_path: None,
                },
            ),
            (
                "empty",
                Capabilities {
                    flags: CapabilityFlags::empty(),
                    max_path: None,
                },
            ),
            (
                "with_trash",
                Capabilities {
                    flags: CapabilityFlags::TRASH,
                    max_path: None,
                },
            ),
            (
                "remote_append_random_write",
                Capabilities {
                    flags: CapabilityFlags::APPEND | CapabilityFlags::RANDOM_WRITE,
                    max_path: None,
                },
            ),
        ],
    );
}

#[test]
fn golden_error() {
    check_family(
        "error.json",
        &[
            ("not_found", Error::NotFound),
            ("permission_denied", Error::PermissionDenied),
            ("loop", Error::Loop),
            ("corrupt", Error::Corrupt),
            (
                "limit_exceeded_entries",
                Error::LimitExceeded {
                    limit: Error::LIMIT_ENTRIES.into(),
                },
            ),
            (
                "limit_exceeded_decompressed_bytes",
                Error::LimitExceeded {
                    limit: Error::LIMIT_DECOMPRESSED_BYTES.into(),
                },
            ),
            (
                "conflict_exists",
                Error::Conflict {
                    conflict: ConflictKind::Exists,
                },
            ),
            (
                "conflict_case_collision",
                Error::Conflict {
                    conflict: ConflictKind::CaseCollision,
                },
            ),
            (
                "conflict_normalization",
                Error::Conflict {
                    conflict: ConflictKind::Normalization,
                },
            ),
            (
                "conflict_type_mismatch",
                Error::Conflict {
                    conflict: ConflictKind::TypeMismatch,
                },
            ),
            (
                "provider_unavailable_retryable",
                Error::ProviderUnavailable { retryable: true },
            ),
            (
                "provider_unavailable_fatal",
                Error::ProviderUnavailable { retryable: false },
            ),
            ("no_space", Error::NoSpace),
            ("io_retryable", Error::Io { retryable: true }),
            ("io_fatal", Error::Io { retryable: false }),
            ("cancelled", Error::Cancelled),
            (
                "policy_denied",
                Error::PolicyDenied {
                    rule: "no_delete_home".to_owned(),
                },
            ),
            ("encoding_loss", Error::EncodingLoss),
            ("unsupported", Error::Unsupported),
            ("invalid_path", Error::InvalidPath),
            ("internal_panic", Error::Internal { panic: true }),
            ("internal_no_panic", Error::Internal { panic: false }),
            (
                "host_key_unknown",
                Error::HostKeyUnknown {
                    host: "sftp.example.com".to_owned(),
                    port: Some(22),
                    algo: "ssh-ed25519".to_owned(),
                    fingerprint: "SHA256:abc123def456".to_owned(),
                },
            ),
            (
                "host_key_mismatch",
                Error::HostKeyMismatch {
                    host: "sftp.example.com".to_owned(),
                    port: None,
                    algo: "ssh-ed25519".to_owned(),
                    fingerprint: "SHA256:zzz999".to_owned(),
                },
            ),
            ("cursor_expired", Error::CursorExpired),
            // 0.36.0 (batch rename): las dos negativas del ejecutor. Ambas
            // significan «no se intentó nada», y ambas son accionables desde el
            // frontend (re-planificar).
            ("plan_stale", Error::PlanStale),
            ("plan_not_executable", Error::PlanNotExecutable),
        ],
    );
}

#[test]
fn golden_task_state() {
    check_family(
        "task_state.json",
        &[
            ("pending", TaskState::Pending),
            ("running", TaskState::Running),
            ("paused", TaskState::Paused),
            ("completed", TaskState::Completed),
            ("cancelled", TaskState::Cancelled),
            (
                "failed_conflict",
                TaskState::Failed {
                    error: Error::Conflict {
                        conflict: ConflictKind::Exists,
                    },
                },
            ),
            (
                "failed_panic",
                TaskState::Failed {
                    error: Error::Internal { panic: true },
                },
            ),
        ],
    );
}

#[test]
fn golden_task_progress() {
    check_family(
        "task_progress.json",
        &[
            (
                "running_mid_copy",
                TaskProgress {
                    task_id: TaskId::new(7),
                    kind: TaskKind::Copy,
                    state: TaskState::Running,
                    bytes_done: 1024,
                    bytes_total: Some(4096),
                    entries_done: 1,
                    entries_total: Some(3),
                    current: Some(vpath("file:///src/informe%FF%FE.dat")),
                },
            ),
            (
                // 0.33.0 (M4-IA-2): TaskKind::Embed en el wire.
                "running_embed",
                TaskProgress {
                    task_id: TaskId::new(11),
                    kind: TaskKind::Embed,
                    state: TaskState::Running,
                    bytes_done: 0,
                    bytes_total: None,
                    entries_done: 2,
                    entries_total: Some(10),
                    current: Some(vpath("file:///home/user/doc.txt")),
                },
            ),
            (
                // 0.36.0 (batch rename): TaskKind::RenameBatch en el wire, y
                // con él la SEMÁNTICA del progreso de un lote — `entries_*`
                // cuenta PASOS del plan (2 de 3), y `bytes_*` es `None` porque
                // un rename no mueve bytes. Un frontend que pintara una barra
                // de bytes aquí pintaría cero para siempre.
                "running_rename_batch",
                TaskProgress {
                    task_id: TaskId::new(13),
                    kind: TaskKind::RenameBatch,
                    state: TaskState::Running,
                    bytes_done: 0,
                    bytes_total: None,
                    entries_done: 2,
                    entries_total: Some(3),
                    current: Some(vpath("file:///home/user/fotos/informe%FF%FE.dat")),
                },
            ),
            (
                // 0.31.0 (#104): TaskKind::Mkdir en el wire.
                "running_mkdir",
                TaskProgress {
                    task_id: TaskId::new(9),
                    kind: TaskKind::Mkdir,
                    state: TaskState::Running,
                    bytes_done: 0,
                    bytes_total: None,
                    entries_done: 0,
                    entries_total: Some(1),
                    current: Some(vpath("file:///tmp/nueva-carpeta")),
                },
            ),
            (
                "pending_unknown_totals",
                TaskProgress {
                    task_id: TaskId::new(1),
                    kind: TaskKind::Move,
                    state: TaskState::Pending,
                    bytes_done: 0,
                    bytes_total: None,
                    entries_done: 0,
                    entries_total: None,
                    current: None,
                },
            ),
            (
                "terminal_cancelled",
                TaskProgress {
                    task_id: TaskId::new(3),
                    kind: TaskKind::Delete,
                    state: TaskState::Cancelled,
                    bytes_done: 512,
                    bytes_total: Some(4096),
                    entries_done: 0,
                    entries_total: Some(2),
                    current: None,
                },
            ),
        ],
    );
}

/// Los tipos SUELTOS del batch de renames (0.36.0): la pareja pedida, el paso
/// del plan y el veredicto. Todos llevan nombres BASE como `Segment`, así que
/// cada fixture demuestra además que un nombre no-UTF8 viaja percent-encoded y
/// vuelve byte a byte (regla dura 1).
#[test]
fn golden_rename_batch_types() {
    use norte_proto::methods::{RenameCollision, RenameCollisionKind, RenamePair, RenameStep};
    check_family(
        "rename_pair.json",
        &[
            (
                "plain",
                RenamePair {
                    from: seg(b"ep1.mkv"),
                    to: seg(b"ep01.mkv"),
                },
            ),
            // Nombre HOSTIL a los dos lados: el wire lo escapa, el round-trip
            // devuelve los bytes exactos.
            (
                "hostile",
                RenamePair {
                    from: seg(b"caf\xff.txt"),
                    to: seg(b"caf\xfe.txt"),
                },
            ),
        ],
    );
    check_family(
        "rename_step.json",
        &[
            (
                "plain",
                RenameStep {
                    from: seg(b"b"),
                    to: seg(b"a"),
                    temp: false,
                },
            ),
            // Los DOS pasos de maquinaria: entrar al temporal y SALIR de él.
            // `temp` es del PASO, no de `to` — el que saca al fichero lo lleva
            // en `from` y es maquinaria igual, así que ambos se pinean.
            (
                "to_temp",
                RenameStep {
                    from: seg(b"a"),
                    to: seg(b".norte-rename-0a1b2c3d-0"),
                    temp: true,
                },
            ),
            (
                "from_temp",
                RenameStep {
                    from: seg(b".norte-rename-0a1b2c3d-0"),
                    to: seg(b"b"),
                    temp: true,
                },
            ),
            (
                "hostile",
                RenameStep {
                    from: seg(b"caf\xff.txt"),
                    to: seg(b"caf\xfe.txt"),
                    temp: false,
                },
            ),
        ],
    );
    // Las TRES clases del vocabulario cerrado, una fixture cada una. Que sigan
    // siendo TODAS no lo garantiza este `check_family` — compara fixtures
    // contra esta lista escrita a mano, así que una cuarta variante sin ninguna
    // de las dos cosas pasa desapercibida —, sino el cruce contra el artefacto
    // en `schema.rs`
    // (`el_schema_de_rename_collision_kind_cubre_los_veredictos_de_la_golden`),
    // que sí se genera del tipo.
    //
    // Cada una con `pair_index` DISTINTO: es el campo que no depende de `kind`
    // y el que permite señalar la fila culpable bajo un veredicto que el
    // cliente no entiende.
    check_family(
        "rename_collision.json",
        &[
            (
                "absent_source",
                RenameCollision {
                    pair_index: 0,
                    name: seg(b"ep7.mkv"),
                    kind: RenameCollisionKind::AbsentSource,
                },
            ),
            (
                "external",
                RenameCollision {
                    pair_index: 2,
                    name: seg(b"caf\xff.txt"),
                    kind: RenameCollisionKind::External,
                },
            ),
            (
                "internal",
                RenameCollision {
                    pair_index: 1,
                    name: seg(b"ep01.mkv"),
                    kind: RenameCollisionKind::Internal,
                },
            ),
            // El origen SOBRA en vez de faltar: el nombre pedido se pliega
            // sobre dos entradas del directorio y no coincide exacto con
            // ninguna. `name` es el origen tal como lo escribió quien pidió.
            (
                "ambiguous_source",
                RenameCollision {
                    pair_index: 3,
                    // NFD ON PURPOSE, and escaped on both sides: this
                    // verdict exists FOR the NFC/NFD twin, and NFC is
                    // the one spelling a stray normalisation pass
                    // would leave untouched. The fixture writes it
                    // `\u0301` so no editor can undo it.
                    name: seg("cafe\u{301}".as_bytes()),
                    kind: RenameCollisionKind::AmbiguousSource,
                },
            ),
        ],
    );
}

/// El plan es wire-frozen: un paso, una colisión y el hash tienen nombres de
/// campo fijos, y los nombres son segmentos percent-encoded.
#[test]
fn golden_fs_rename_batch_plan_result() {
    use norte_proto::methods::{
        FsRenameBatchPlanResult, RenameCollision, RenameCollisionKind, RenameStep,
    };
    check_family(
        "fs_rename_batch_plan_result.json",
        &[
            // LA FORMA DE REFERENCIA: la permutación `a→b, b→a`, el caso que
            // hoy es imposible con N `fs.move` sueltos. Son TRES pasos, y el
            // tercero es el que la cierra: sin `.norte-rename-… → b`, el
            // fichero que empezó como `a` se queda aparcado bajo el nombre de
            // máquina y `b` nunca llega a existir. Contra esta fixture se
            // escribe el planificador, así que un plan truncado aquí sería un
            // planificador truncado allí.
            (
                "permutation",
                FsRenameBatchPlanResult {
                    steps: vec![
                        RenameStep {
                            from: seg(b"a"),
                            to: seg(b".norte-rename-0a1b2c3d-0"),
                            temp: true,
                        },
                        RenameStep {
                            from: seg(b"b"),
                            to: seg(b"a"),
                            temp: false,
                        },
                        RenameStep {
                            from: seg(b".norte-rename-0a1b2c3d-0"),
                            to: seg(b"b"),
                            temp: true,
                        },
                    ],
                    collisions: vec![],
                    executable: true,
                    plan_hash: plan_hash(&"2".repeat(64)),
                },
            ),
            // El plan MUERTO, y su forma importa tanto como la de arriba:
            // `steps` VACÍO. Un plan con veredictos no se ordena a medias — el
            // planificador no emite pasos para él —, así que un caller que
            // ignorase `executable` no tendría nada que ejecutar de todos
            // modos. Pasos y colisiones NO coexisten en nada que emita el core
            // (decisión 4 del diseño), y una fixture que los mezclara pinearía
            // una forma que no existe.
            //
            // Dos veredictos sobre parejas distintas: el índice es lo que hace
            // señalable la fila, y el nombre hostil viaja percent-encoded.
            (
                "not_executable",
                FsRenameBatchPlanResult {
                    steps: vec![],
                    collisions: vec![
                        RenameCollision {
                            pair_index: 1,
                            name: seg(b"ep01.mkv"),
                            kind: RenameCollisionKind::Internal,
                        },
                        RenameCollision {
                            pair_index: 2,
                            name: seg(b"caf\xff.txt"),
                            kind: RenameCollisionKind::External,
                        },
                    ],
                    executable: false,
                    plan_hash: plan_hash(&"0".repeat(64)),
                },
            ),
            // El plan ejecutable: `collisions` vacío es una LISTA VACÍA en el
            // wire, jamás una clave ausente ni `null`. El hash es SINTÉTICO a
            // propósito: un sha256 real de algo — el del input vacío, por
            // ejemplo — dejaría pasar este golden a un hasher que no alimentara
            // nada.
            (
                "executable",
                FsRenameBatchPlanResult {
                    steps: vec![RenameStep {
                        from: seg(b"ep1.mkv"),
                        to: seg(b"ep01.mkv"),
                        temp: false,
                    }],
                    collisions: vec![],
                    executable: true,
                    plan_hash: plan_hash(&"1".repeat(64)),
                },
            ),
        ],
    );
}

#[test]
fn golden_methods() {
    let fixtures = load("methods.json");
    check_methods_fs(&fixtures);
    check_methods_daemon(&fixtures);
    check_methods_v05(&fixtures);
    check_methods_connection(&fixtures);
    check_methods_policy(&fixtures);
    check_methods_session(&fixtures);
    check_methods_plugin(&fixtures);
    check_methods_rpc(&fixtures);
    check_methods_index(&fixtures);
    check_methods_ai(&fixtures);
    check_methods_rename_batch(&fixtures);
    // 98 → 101 en 0.32.0: + ai_rename_plan_params/result/result_empty (M4-IA,
    // ADR 0031). 101 → 106 en 0.33.0: + index_embed_params,
    // index_search_semantic_params(/_no_root)/result y semantic_hit (M4-IA-2,
    // ADR 0031 A3). 106 → 113 en 0.34.0: + plugin_info_with_help,
    // plugin_help_params y
    // plugin_help_result(/_flags/_empty/_lossy/_absent) (H3e, ADR 0040 — los
    // tres últimos son la página vacía, la hostil y el campo AUSENTE).
    // 113 → 114 en 0.35.0: + plugin_column_values_params_scoped (#120 — la
    // petición que NOMBRA al plugin; la que no lo nombra conserva su fixture
    // byte a byte, que es lo que `skip_serializing_if` promete).
    // 114 → 116 en 0.36.0: + fs_rename_batch_plan_params y fs_rename_batch_params
    // (el batch de renames; el RESULT del plan tiene fichero propio, porque su
    // familia pinea varias formas de plan).
    assert_eq!(fixtures.len(), 116, "[methods.json] fixtures sin caso Rust");
}

/// Familia `fs.rename_batch*` (0.36.0): las PETICIONES de plan y de ejecución.
/// La intención (`pairs`) es lo único que el cliente manda — el orden lo decide
/// el core —, y la ejecución añade el `plan_hash` que el humano aprobó.
fn check_methods_rename_batch(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{FsRenameBatchParams, FsRenameBatchPlanParams, RenamePair};
    // Dir HOSTIL + una permutación `a→b, b→a`: el caso que motiva el método.
    check_one(
        fixtures,
        "fs_rename_batch_plan_params",
        &FsRenameBatchPlanParams {
            dir: vpath("file:///home/user/fotos-a%FF%FE"),
            pairs: vec![
                RenamePair {
                    from: seg(b"a"),
                    to: seg(b"b"),
                },
                RenamePair {
                    from: seg(b"b"),
                    to: seg(b"a"),
                },
            ],
        },
    );
    // La petición de EJECUCIÓN apunta al plan EJECUTABLE de
    // `fs_rename_batch_plan_result.json`: mismas parejas y su mismo hash. Con
    // el del plan muerto — `"0"×64` — esta fixture sería una petición de
    // aspecto legal para un plan que el core tiene que rechazar, y quien
    // copiara la fixture a un test de la task 7 escribiría ese test al revés.
    check_one(
        fixtures,
        "fs_rename_batch_params",
        &FsRenameBatchParams {
            dir: vpath("file:///home/user/fotos-a%FF%FE"),
            pairs: vec![RenamePair {
                from: seg(b"ep1.mkv"),
                to: seg(b"ep01.mkv"),
            }],
            plan_hash: plan_hash(&"1".repeat(64)),
        },
    );
}

/// Familia `ai.*` (0.32.0, M4-IA, ADR 0031): plan de rename revisable.
fn check_methods_ai(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::VPath;
    use norte_proto::methods::{AiRenameEntry, AiRenamePlanParams, AiRenamePlanResult};
    // Dir HOSTIL (no-UTF8, percent-encoded en el wire de VPath).
    check_one(
        fixtures,
        "ai_rename_plan_params",
        &AiRenamePlanParams {
            dir: VPath::parse("file:///home/user/fotos-a%FF%FE").unwrap(),
            instruction: "kebab-case, date first".into(),
        },
    );
    check_one(
        fixtures,
        "ai_rename_plan_result",
        &AiRenamePlanResult {
            entries: vec![AiRenameEntry {
                from: "IMG 001.jpg".into(),
                to: "2024-01-01-beach.jpg".into(),
            }],
        },
    );
    // Plan vacío = el modelo no propuso cambios (estado significativo, no un
    // caso omitido): fija la forma del wire, no solo el caso feliz.
    check_one(
        fixtures,
        "ai_rename_plan_result_empty",
        &AiRenamePlanResult { entries: vec![] },
    );
}

/// Familia `index.*` (0.25.0, M4, ADR 0034): build + query del índice de búsqueda.
fn check_methods_index(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        IndexBuildParams, IndexBuildResult, IndexEmbedParams, IndexHit, IndexQueryParams,
        IndexQueryResult, IndexSearchSemanticParams, IndexSearchSemanticResult, SemanticHit,
    };
    use norte_proto::{EntryKind, VPath};
    check_one(
        fixtures,
        "index_build_params",
        &IndexBuildParams {
            root: VPath::parse("file:///home/user").unwrap(),
        },
    );
    check_one(
        fixtures,
        "index_build_result",
        &IndexBuildResult {
            indexed: 128,
            removed: 3,
        },
    );
    check_one(
        fixtures,
        "index_query_params",
        &IndexQueryParams {
            root: VPath::parse("file:///home/user").unwrap(),
            text: "informe anual".into(),
            limit: 50,
        },
    );
    // Un hit con nombre HOSTIL (no-UTF8, percent-encoded en el wire de VPath).
    check_one(
        fixtures,
        "index_hit",
        &IndexHit {
            path: VPath::parse("file:///home/user/informe-a%FF%FE.txt").unwrap(),
            kind: EntryKind::File,
            size: Some(4096),
            mtime_ms: Some(1_700_000_000_000),
        },
    );
    check_one(
        fixtures,
        "index_query_result",
        &IndexQueryResult {
            hits: vec![IndexHit {
                path: VPath::parse("file:///home/user/informe-anual.txt").unwrap(),
                kind: EntryKind::File,
                size: Some(4096),
                mtime_ms: None,
            }],
        },
    );
    // 0.33.0 (M4-IA-2, ADR 0031 A3): index.embed (Task) + index.search_semantic
    // (directa cancelable). Scores EXACTOS en binario (0.5) para que el
    // round-trip de f64 no tenga nada que redondear.
    check_one(
        fixtures,
        "index_embed_params",
        &IndexEmbedParams {
            root: VPath::parse("file:///home/user").unwrap(),
        },
    );
    check_one(
        fixtures,
        "index_search_semantic_params",
        &IndexSearchSemanticParams {
            root: Some(VPath::parse("file:///home/user").unwrap()),
            query: "informe anual".into(),
            k: 20,
        },
    );
    // Pinea la AUSENCIA de `root` en el wire (default + skip_serializing_if):
    // sin la clave, no `"root": null`.
    check_one(
        fixtures,
        "index_search_semantic_params_no_root",
        &IndexSearchSemanticParams {
            root: None,
            query: "informe".into(),
            k: 20,
        },
    );
    // Un hit con nombre HOSTIL (no-UTF8, percent-encoded en el wire de VPath).
    check_one(
        fixtures,
        "semantic_hit",
        &SemanticHit {
            path: VPath::parse("file:///home/user/informe-a%FF%FE.txt").unwrap(),
            score: 0.5,
        },
    );
    check_one(
        fixtures,
        "index_search_semantic_result",
        &IndexSearchSemanticResult {
            hits: vec![SemanticHit {
                path: VPath::parse("file:///home/user/a.txt").unwrap(),
                score: 0.5,
            }],
        },
    );
}

/// Familia de la CAPA RPC (0.19.0, #72): `rpc.cancel`.
fn check_methods_rpc(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::RpcCancelParams;
    use norte_proto::wire::RequestId;
    check_one(
        fixtures,
        "rpc_cancel_params",
        &RpcCancelParams {
            id: RequestId::Num(7),
        },
    );
}

/// Familia plugin.* (0.13.0, M4-P3): catálogo + aprobación/activación humanas.
fn check_methods_plugin(fixtures: &BTreeMap<String, Value>) {
    check_methods_plugin_governance(fixtures);
    check_methods_plugin_exec(fixtures);
    check_methods_plugin_data_out_v2(fixtures);
    check_methods_plugin_config(fixtures);
    check_methods_plugin_help(fixtures);
}

/// Casos de `plugin.help` (H3e, 0.34.0): el `has_help` de [`PluginInfo`] y
/// los dos tipos del método. Función propia por el límite de líneas de
/// `check_methods_plugin_info`, cuyos goldens NO cambian — que sigan byte a
/// byte iguales es justo lo que demuestra que el campo es aditivo fuerte.
fn check_methods_plugin_help(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{PluginHelpParams, PluginHelpResult, PluginInfo};
    check_one(
        fixtures,
        "plugin_info_with_help",
        &PluginInfo {
            id: "acme.ftp".to_owned(),
            name: "FTP".to_owned(),
            publisher: "ACME".to_owned(),
            version: "0.1.0".to_owned(),
            category: "provider".to_owned(),
            capabilities: vec!["fs-read".to_owned()],
            approved: true,
            enabled: true,
            description: None,
            commands: Vec::new(),
            columns: Vec::new(),
            has_help: true,
        },
    );
    check_one(
        fixtures,
        "plugin_help_params",
        &PluginHelpParams {
            id: "acme.ftp".to_owned(),
        },
    );
    check_one(
        fixtures,
        "plugin_help_result",
        &PluginHelpResult {
            markdown: "+++\ntitle = \"FTP\"\n+++\nBody.".to_owned(),
            truncated: false,
            lossy: false,
        },
    );
    check_one(
        fixtures,
        "plugin_help_result_flags",
        &PluginHelpResult {
            markdown: "cut".to_owned(),
            truncated: true,
            lossy: true,
        },
    );
    // La forma "no hay página" que el contrato PROMETE: cadena vacía y ambas
    // banderas bajas, nunca un error (ver el rustdoc de `markdown`).
    check_one(
        fixtures,
        "plugin_help_result_empty",
        &PluginHelpResult {
            markdown: String::new(),
            truncated: false,
            lossy: false,
        },
    );
    // HOSTIL, y banderas MIXTAS (cabe perder bytes sin llegar al tope): el
    // `U+FFFD` que `lossy` describe viaja VERBATIM, y con él un override
    // bidi `U+202E` que da la vuelta al texto que le sigue ("gnp.exe" se lee
    // "exe.png"). Enmascarar es cosa del FRONTEND al renderizar — el wire
    // transporta, no sanea —, así que la fixture conserva el peligro a
    // propósito: si algún día alguien "limpia" el texto en proto, este
    // golden es lo que se pone rojo.
    check_one(
        fixtures,
        "plugin_help_result_lossy",
        &PluginHelpResult {
            markdown: "Ver\u{FFFD}sion \u{202E}gnp.exe".to_owned(),
            truncated: false,
            lossy: true,
        },
    );
    // `markdown` AUSENTE se lee como la página vacía — normativo desde el
    // rustdoc del campo, y hasta ahora sin fixture. NO va por `check_one`: es
    // deliberadamente asimétrico (en emisión el campo no se omite jamás), así
    // que solo se comprueba la dirección que el contrato promete, la de
    // ENTRADA.
    let absent: PluginHelpResult = serde_json::from_value(
        fixtures
            .get("plugin_help_result_absent")
            .expect("[methods.json] falta la fixture plugin_help_result_absent")
            .clone(),
    )
    .expect("[methods/plugin_help_result_absent] deserialize");
    assert_eq!(
        absent,
        PluginHelpResult {
            markdown: String::new(),
            truncated: false,
            lossy: false,
        },
        "un peer que omite `markdown` está diciendo «no hay página»"
    );
}

/// Casos de [`PluginInfo`]/[`PluginCommandInfo`] (P1, 0.26.0): el shape sin
/// `description`/`commands`, el shape CON ambos poblados, y el tipo suelto
/// `PluginCommandInfo`. Función propia para no desbordar el límite de
/// líneas de `check_methods_plugin_governance`.
fn check_methods_plugin_info(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        PluginColumnInfo, PluginCommandInfo, PluginInfo, PluginListResult, PluginLoadError,
    };
    check_one(
        fixtures,
        "plugin_command_info",
        &PluginCommandInfo {
            id: "greet".into(),
            title: "Greet".into(),
        },
    );
    check_one(
        fixtures,
        "plugin_column_info",
        &PluginColumnInfo {
            id: "git-status".into(),
            header: "Git".into(),
        },
    );
    check_one(
        fixtures,
        "plugin_info",
        &PluginInfo {
            id: "org.norte.demo".into(),
            name: "Demo Previewer".into(),
            publisher: "norte".into(),
            version: "0.1.0".into(),
            category: "previewer".into(),
            capabilities: vec!["fs-read".into()],
            approved: true,
            enabled: true,
            description: None,
            commands: vec![],
            columns: vec![],
            has_help: false,
        },
    );
    // (P1/G3c) description + commands + columns POBLADOS: golden nuevo, no
    // reemplaza al de arriba (que sigue cubriendo el shape sin ellos).
    check_one(
        fixtures,
        "plugin_info_with_commands",
        &PluginInfo {
            id: "org.norte.demo".into(),
            name: "Demo Previewer".into(),
            publisher: "norte".into(),
            version: "0.1.0".into(),
            category: "previewer".into(),
            capabilities: vec!["fs-read".into()],
            approved: true,
            enabled: true,
            description: Some("Previsualiza Markdown en línea.".into()),
            commands: vec![
                PluginCommandInfo {
                    id: "greet".into(),
                    title: "Greet".into(),
                },
                PluginCommandInfo {
                    id: "wave".into(),
                    title: "Wave".into(),
                },
            ],
            columns: vec![PluginColumnInfo {
                id: "git-status".into(),
                header: "Git".into(),
            }],
            has_help: false,
        },
    );
    check_one(
        fixtures,
        "plugin_list_result",
        &PluginListResult {
            plugins: vec![PluginInfo {
                id: "org.norte.demo".into(),
                name: "Demo Previewer".into(),
                publisher: "norte".into(),
                version: "0.1.0".into(),
                category: "previewer".into(),
                capabilities: vec!["fs-read".into()],
                approved: false,
                enabled: false,
                description: None,
                commands: vec![],
                columns: vec![],
                has_help: false,
            }],
            errors: vec![PluginLoadError {
                dir: "/plugins/broken".into(),
                reason: "manifiesto inválido".into(),
            }],
        },
    );
}

/// Familia `plugin.*` de GESTIÓN (0.13.0, M4-P3): listar y aprobar/activar.
fn check_methods_plugin_governance(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        PluginListParams, PluginSetApprovalParams, PluginSetApprovalResult, PluginSetEnabledParams,
        PluginSetEnabledResult,
    };
    // `plugin.list` sin params: golden vacío, simetría con `task_list_params`.
    check_one(fixtures, "plugin_list_params", &PluginListParams {});
    check_methods_plugin_info(fixtures);
    check_one(
        fixtures,
        "plugin_set_approval_params",
        &PluginSetApprovalParams {
            id: "org.norte.demo".into(),
            approved: true,
        },
    );
    check_one(
        fixtures,
        "plugin_set_approval_result",
        &PluginSetApprovalResult {},
    );
    check_one(
        fixtures,
        "plugin_set_enabled_params",
        &PluginSetEnabledParams {
            id: "org.norte.demo".into(),
            enabled: false,
        },
    );
    check_one(
        fixtures,
        "plugin_set_enabled_result",
        &PluginSetEnabledResult {},
    );
}

/// Familia `plugin.*` de EJECUCIÓN (0.14.0/0.15.0, M4-P4/P5): ejecutar y previsualizar.
fn check_methods_plugin_exec(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        PluginPreview, PluginPreviewParams, PluginPreviewResult, PluginRunCommandParams,
        PluginRunCommandResult,
    };
    // `plugin.run_command` (0.14.0, M4-P4): `arg` sin skip → siempre en el wire.
    check_one(
        fixtures,
        "plugin_run_command_params",
        &PluginRunCommandParams {
            id: "org.norte.demo".into(),
            command: "greet".into(),
            arg: "world".into(),
        },
    );
    check_one(
        fixtures,
        "plugin_run_command_result",
        &PluginRunCommandResult {
            output: "hello, world".into(),
        },
    );
    // `plugin.preview` (0.15.0, M4-P5): result poblado (flatten al raíz) y el
    // vacío (`None` → `{}`); un parcial es inexpresable (test en types.rs).
    check_one(
        fixtures,
        "plugin_preview_params",
        &PluginPreviewParams {
            path: vpath("file:///home/user/doc.md"),
        },
    );
    check_one(
        fixtures,
        "plugin_preview_result",
        &PluginPreviewResult {
            preview: Some(PluginPreview {
                plugin_id: "org.norte.md".into(),
                plugin_name: "Markdown Preview".into(),
                output: "<h1>Título</h1>".into(),
                lossy: false,
            }),
        },
    );
    // 0.29.0 (#101): el aviso de decodificación lossy poblado (`true`).
    check_one(
        fixtures,
        "plugin_preview_result_lossy",
        &PluginPreviewResult {
            preview: Some(PluginPreview {
                plugin_id: "org.norte.md".into(),
                plugin_name: "Markdown Preview".into(),
                output: "a\u{fffd}b".into(),
                lossy: true,
            }),
        },
    );
    check_one(
        fixtures,
        "plugin_preview_result_none",
        &PluginPreviewResult { preview: None },
    );
}

/// Familia `plugin.*` de CONFIGURACIÓN (0.28.0, G3c, ADR 0037): esquema +
/// valor efectivo (`plugin.get_config`) y persistir un valor
/// (`plugin.set_config`). `plugin_config_key_wire_bare` cubre el shape
/// mínimo (min/max/description ausentes, values vacío — todos
/// `skip_serializing_if`/aditivos siempre presentes según corresponda);
/// `plugin_config_key_wire_full` el shape con TODO poblado (tipo `int` con
/// min/max, que son los únicos campos opcionales de la struct).
fn check_methods_plugin_config(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        PluginConfigKeyWire, PluginGetConfigParams, PluginGetConfigResult, PluginSetConfigParams,
        PluginSetConfigResult,
    };
    check_one(
        fixtures,
        "plugin_get_config_params",
        &PluginGetConfigParams {
            id: "org.norte.demo".into(),
        },
    );
    check_one(
        fixtures,
        "plugin_config_key_wire_bare",
        &PluginConfigKeyWire {
            key: "greeting".into(),
            kind: "string".into(),
            default: "hola".into(),
            min: None,
            max: None,
            values: vec![],
            description: None,
            value: "hola".into(),
        },
    );
    check_one(
        fixtures,
        "plugin_config_key_wire_full",
        &PluginConfigKeyWire {
            key: "retries".into(),
            kind: "int".into(),
            default: "3".into(),
            min: Some(0),
            max: Some(10),
            values: vec![],
            description: Some("Número de reintentos.".into()),
            value: "5".into(),
        },
    );
    check_one(
        fixtures,
        "plugin_get_config_result",
        &PluginGetConfigResult {
            keys: vec![
                PluginConfigKeyWire {
                    key: "greeting".into(),
                    kind: "string".into(),
                    default: "hola".into(),
                    min: None,
                    max: None,
                    values: vec![],
                    description: None,
                    value: "hola".into(),
                },
                PluginConfigKeyWire {
                    key: "mode".into(),
                    kind: "enum".into(),
                    default: "fast".into(),
                    min: None,
                    max: None,
                    values: vec!["fast".into(), "thorough".into()],
                    description: None,
                    value: "thorough".into(),
                },
            ],
        },
    );
    check_one(
        fixtures,
        "plugin_get_config_result_empty",
        &PluginGetConfigResult { keys: vec![] },
    );
    check_one(
        fixtures,
        "plugin_set_config_params",
        &PluginSetConfigParams {
            id: "org.norte.demo".into(),
            key: "mode".into(),
            value: "thorough".into(),
        },
    );
    check_one(
        fixtures,
        "plugin_set_config_result",
        &PluginSetConfigResult {},
    );
}

/// Familia `plugin.*` de datos ESTRUCTURADOS v2 (0.27.0, G3, ADR 0037): el
/// host pinta, nunca el plugin. Cubre preview con estilo (mismo patrón
/// `flatten`-sobre-`Option` all-or-nothing que [`PluginPreviewResult`]),
/// decoraciones POSICIONALES 1:1 y columnas POSICIONALES 1:1. Repartida en
/// dos funciones (preview con estilo / decorate+columns) por el límite de
/// líneas de clippy, mismo criterio que `check_methods_plugin_info`.
fn check_methods_plugin_data_out_v2(fixtures: &BTreeMap<String, Value>) {
    check_methods_plugin_preview_styled(fixtures);
    check_methods_plugin_decorate_and_columns(fixtures);
}

/// `plugin.preview_styled` (0.27.0): mismo patrón all-or-nothing que
/// `plugin.preview`, con `lines: Vec<Vec<SpanWire>>` en vez de `output: String`.
fn check_methods_plugin_preview_styled(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::PluginPreviewStyledResult;
    use norte_proto::methods::{PluginPreviewStyled, PluginPreviewStyledParams, SpanWire};
    // Un span suelto: shape mínimo (solo `text`, role/fg omitidos por
    // `skip_serializing_if`) y el shape POBLADO (role+fg juntos en el wire,
    // aunque el host pinte con `role` cuando ambos están presentes).
    check_one(
        fixtures,
        "span_wire_bare",
        &SpanWire {
            text: "fn".into(),
            role: None,
            fg: None,
        },
    );
    check_one(
        fixtures,
        "span_wire_styled",
        &SpanWire {
            text: "año".into(),
            role: Some("match".into()),
            fg: Some([200, 40, 40]),
        },
    );
    check_one(
        fixtures,
        "plugin_preview_styled_params",
        &PluginPreviewStyledParams {
            path: vpath("file:///home/user/doc.rs"),
        },
    );
    check_one(
        fixtures,
        "plugin_preview_styled_result",
        &PluginPreviewStyledResult {
            preview: Some(PluginPreviewStyled {
                plugin_id: "org.norte.demo".into(),
                plugin_name: "Demo Previewer".into(),
                lines: vec![
                    vec![
                        SpanWire {
                            text: "fn".into(),
                            role: Some("match".into()),
                            fg: None,
                        },
                        SpanWire {
                            text: " main".into(),
                            role: None,
                            fg: None,
                        },
                    ],
                    vec![SpanWire {
                        text: "año".into(),
                        role: None,
                        fg: Some([255, 0, 0]),
                    }],
                ],
                lossy: false,
            }),
        },
    );
    // 0.29.0 (#101): paridad con `plugin_preview_result_lossy` — la variante
    // con estilo también pinea `lossy: true` en el wire.
    check_one(
        fixtures,
        "plugin_preview_styled_result_lossy",
        &PluginPreviewStyledResult {
            preview: Some(PluginPreviewStyled {
                plugin_id: "org.norte.demo".into(),
                plugin_name: "Demo Previewer".into(),
                lines: vec![vec![SpanWire {
                    text: "a\u{fffd}b".into(),
                    role: None,
                    fg: None,
                }]],
                lossy: true,
            }),
        },
    );
    check_one(
        fixtures,
        "plugin_preview_styled_result_none",
        &PluginPreviewStyledResult { preview: None },
    );
}

/// `plugin.decorate` + `plugin.column_values` (0.27.0): ambos POSICIONALES
/// 1:1 con `params.paths`. `plugin_decorate_params` incluye un nombre HOSTIL
/// (no-UTF8); el segundo elemento de `plugin_decorate_result` es `{}` (sin
/// badge/role de ESE plugin para ESA entrada), no un elemento omitido.
fn check_methods_plugin_decorate_and_columns(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        DecorationWire, PluginColumnValuesParams, PluginColumnValuesResult, PluginDecorateParams,
        PluginDecorateResult, PluginDecorations,
    };
    check_one(
        fixtures,
        "plugin_decorate_params",
        &PluginDecorateParams {
            paths: vec![
                vpath("file:///repo/a.rs"),
                vpath("file:///repo/informe%FF%FE.dat"),
            ],
        },
    );
    check_one(
        fixtures,
        "decoration_wire",
        &DecorationWire {
            badge: Some("M".into()),
            role: Some("warning".into()),
        },
    );
    check_one(
        fixtures,
        "decoration_wire_empty",
        &DecorationWire {
            badge: None,
            role: None,
        },
    );
    check_one(
        fixtures,
        "plugin_decorate_result",
        &PluginDecorateResult {
            plugins: vec![PluginDecorations {
                plugin_id: "org.norte.git".into(),
                decorations: vec![
                    DecorationWire {
                        badge: Some("M".into()),
                        role: Some("warning".into()),
                    },
                    DecorationWire {
                        badge: None,
                        role: None,
                    },
                ],
            }],
        },
    );
    // Ningún decorator respondió: `plugins` vacío. A diferencia del patrón
    // `flatten` de preview, aquí no hay all-or-nothing — un plugin ausente
    // es simplemente un elemento ausente de `plugins`.
    check_one(
        fixtures,
        "plugin_decorate_result_empty",
        &PluginDecorateResult { plugins: vec![] },
    );
    // `paths` lleva DOS entradas para que `values` pueda pinnear ambos casos
    // posicionales: una celda real y una `None` (la columna no aplica a esa
    // entrada, distinguible de una cadena vacía real).
    check_one(
        fixtures,
        "plugin_column_values_params",
        &PluginColumnValuesParams {
            column_id: "git-status".into(),
            paths: vec![vpath("file:///repo/a.rs"), vpath("file:///repo/README")],
            // Sin `plugin_id`: es la petición de un cliente 0.34, y su golden
            // tiene que seguir siendo EL MISMO fichero que antes del bump —
            // eso es lo que `skip_serializing_if` promete.
            plugin_id: None,
        },
    );
    // Con `plugin_id` (0.35.0, #120): la forma nueva, en su propio golden.
    check_one(
        fixtures,
        "plugin_column_values_params_scoped",
        &PluginColumnValuesParams {
            column_id: "status".into(),
            paths: vec![vpath("file:///repo/a.rs")],
            plugin_id: Some("org.norte.git".into()),
        },
    );
    check_one(
        fixtures,
        "plugin_column_values_result",
        &PluginColumnValuesResult {
            values: vec![Some("modified".into()), None],
        },
    );
}

/// Familia session.* (0.12.0, M3-4): undo de sesión de agente por el wire.
fn check_methods_session(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        PolicyUndoReportParams, PolicyUndoReportResult, PolicyUndoSessionParams,
        PolicyUndoSessionResult, UndoBlocked,
    };
    check_one(
        fixtures,
        "policy_undo_session_params",
        &PolicyUndoSessionParams {
            session: "claude".into(),
        },
    );
    check_one(
        fixtures,
        "policy_undo_session_result",
        &PolicyUndoSessionResult {
            task_id: norte_proto::TaskId::new(9),
        },
    );
    // 0.16.0 (#71): el informe del undo por el wire.
    check_one(
        fixtures,
        "policy_undo_report_params",
        &PolicyUndoReportParams {
            task_id: norte_proto::TaskId::new(9),
        },
    );
    check_one(
        fixtures,
        "policy_undo_report_result",
        &PolicyUndoReportResult {
            undone: 3,
            skipped_irreversible: 1,
            skipped_created_no_trash: 2,
            blocked: Some(UndoBlocked {
                seq: 41,
                error: norte_proto::Error::Conflict {
                    conflict: norte_proto::ConflictKind::Exists,
                },
            }),
        },
    );
    // Sin bloqueo: `blocked` se OMITE (skip_serializing_if), no `null`.
    check_one(
        fixtures,
        "policy_undo_report_result_clean",
        &PolicyUndoReportResult {
            undone: 4,
            skipped_irreversible: 0,
            skipped_created_no_trash: 0,
            blocked: None,
        },
    );
}

/// Familia policy.* (0.11.0, M3-3b): scopes + aprobaciones + `agent_session`.
fn check_methods_policy(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        ClientInfo, GrantScopeParams, GrantScopeResult, InitializeParams, PendingApproval,
        PolicyApprovalRequired, PolicyDecideParams, PolicyDecideResult, PolicyPendingResult,
        RequestScopeParams, RequestScopeResult,
    };
    check_one(
        fixtures,
        "initialize_params_agent",
        &InitializeParams {
            client_info: ClientInfo {
                name: "mcp".into(),
                version: "1".into(),
            },
            protocol_version: "0.11.0".into(),
            encodings: vec![],
            agent_session: Some("s1".into()),
        },
    );
    check_one(
        fixtures,
        "request_scope_params",
        &RequestScopeParams {
            session: "s1".into(),
            roots: vec![vpath("file:///work")],
            ops: vec!["copy".into(), "delete".into()],
            ttl_ms: 60_000,
        },
    );
    check_one(
        fixtures,
        "request_scope_result",
        &RequestScopeResult { request_id: 3 },
    );
    check_one(
        fixtures,
        "grant_scope_params",
        &GrantScopeParams { request_id: 3 },
    );
    check_one(fixtures, "grant_scope_result", &GrantScopeResult {});
    check_one(
        fixtures,
        "policy_approval_required",
        &PolicyApprovalRequired {
            approval_id: 7,
            session: Some("s1".into()),
            op: "delete".into(),
            paths: vec!["file:///work/x".into()],
            ttl_ms: 30_000,
        },
    );
    check_one(
        fixtures,
        "policy_decide_params",
        &PolicyDecideParams {
            approval_id: 7,
            approve: true,
        },
    );
    check_one(fixtures, "policy_decide_result", &PolicyDecideResult {});
    check_one(
        fixtures,
        "pending_approval",
        &PendingApproval {
            approval_id: 7,
            session: Some("s1".into()),
            op: "delete".into(),
            paths: vec!["file:///work/x".into()],
        },
    );
    check_one(
        fixtures,
        "policy_pending_result",
        &PolicyPendingResult {
            pending: vec![PendingApproval {
                approval_id: 7,
                session: Some("s1".into()),
                op: "delete".into(),
                paths: vec!["file:///work/x".into()],
            }],
        },
    );
}

/// Familia connection.* (0.7.0, fase 6): `trust_host_key` del flujo TOFU.
fn check_methods_connection(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        ConnectionDegraded, ConnectionTrustHostKeyParams, ConnectionTrustHostKeyResult,
    };
    check_one(
        fixtures,
        "connection_trust_host_key_params",
        &ConnectionTrustHostKeyParams {
            host: "sftp.example.com".to_owned(),
            port: Some(22),
            algo: "ssh-ed25519".to_owned(),
            fingerprint: "SHA256:abc123def456".to_owned(),
        },
    );
    // `port` ausente serializa como `null` (Option sin skip): caso pinneado.
    check_one(
        fixtures,
        "connection_trust_host_key_params_sin_puerto",
        &ConnectionTrustHostKeyParams {
            host: "sftp.example.com".to_owned(),
            port: None,
            algo: "ssh-ed25519".to_owned(),
            fingerprint: "SHA256:abc123def456".to_owned(),
        },
    );
    check_one(
        fixtures,
        "connection_trust_host_key_result",
        &ConnectionTrustHostKeyResult { trusted: true },
    );
    check_one(
        fixtures,
        "connection_degraded",
        &ConnectionDegraded {
            scheme: "ftp".into(),
            host: "backup.example".into(),
            reason: "tls-auth-rejected".into(),
            detail: None,
        },
    );
}

/// Params de `fs.list`/`fs.stat`. Los casos `_con_attrs` (0.30.0, ADR 0039)
/// piden ids; los de al lado, SIN el campo, son la prueba de aditividad: un
/// `attrs` vacío no viaja al wire.
fn check_methods_fs_params(fixtures: &BTreeMap<String, Value>) {
    check_one(
        fixtures,
        "fs_list_params",
        &FsListParams {
            path: vpath("file:///home/user"),
            limit: None,
            cursor: None,
            attrs: Vec::new(),
        },
    );
    check_one(
        fixtures,
        "fs_list_params_paginado",
        &FsListParams {
            path: vpath("file:///home/user"),
            limit: Some(1000),
            cursor: Some("3".to_owned()),
            attrs: Vec::new(),
        },
    );
    check_one(
        fixtures,
        "fs_list_params_con_attrs",
        &FsListParams {
            path: vpath("file:///home/user"),
            limit: Some(500),
            cursor: None,
            attrs: vec!["posix.mode".to_owned(), "posix.uid".to_owned()],
        },
    );
    check_one(
        fixtures,
        "fs_stat_params_con_attrs",
        &FsStatParams {
            path: vpath("file:///home/user/doc.txt"),
            attrs: vec!["s3.storage_class".to_owned()],
        },
    );
    check_one(
        fixtures,
        "fs_stat_params",
        &FsStatParams {
            path: vpath("file:///home/user/doc.txt"),
            attrs: Vec::new(),
        },
    );
}

/// Familia fs.* + task.cancel (list/stat/copy/move/delete/task).
fn check_methods_fs(fixtures: &BTreeMap<String, Value>) {
    let sample_entry = Entry {
        attrs: std::collections::BTreeMap::new(),
        path: vpath("file:///home/user/doc.txt"),
        kind: EntryKind::File,
        size: Some(1234),
        mtime_ms: Some(1_720_000_000_000),
    };
    check_methods_fs_params(fixtures);
    check_one(
        fixtures,
        "fs_list_result",
        &FsListResult {
            entries: vec![sample_entry.clone()],
            next_cursor: None,
            skipped: None,
        },
    );
    check_one(
        fixtures,
        "fs_list_result_paginado",
        &FsListResult {
            entries: vec![sample_entry.clone()],
            next_cursor: Some("3".to_owned()),
            skipped: None,
        },
    );
    check_one(
        fixtures,
        "fs_list_result_con_skipped",
        &FsListResult {
            entries: vec![sample_entry.clone()],
            next_cursor: None,
            skipped: Some(3),
        },
    );
    check_one(
        fixtures,
        "fs_stat_result",
        &FsStatResult {
            entry: sample_entry,
        },
    );
    check_methods_transfer(fixtures);
    check_one(
        fixtures,
        "fs_delete_params",
        &FsDeleteParams {
            path: vpath("file:///tmp/victim"),
            mode: norte_proto::DeleteMode::Trash,
        },
    );
    check_one(
        fixtures,
        "fs_delete_params_permanent",
        &FsDeleteParams {
            path: vpath("file:///tmp/victim"),
            mode: norte_proto::DeleteMode::Permanent,
        },
    );
    check_one(
        fixtures,
        "fs_task_result",
        &FsTaskResult {
            task_id: TaskId::new(7),
        },
    );
    // 0.31.0 (#104): fs.mkdir.
    check_one(
        fixtures,
        "fs_mkdir_params",
        &norte_proto::methods::FsMkdirParams {
            path: vpath("file:///tmp/nueva-carpeta"),
        },
    );
    check_methods_search(fixtures);
    check_one(
        fixtures,
        "task_cancel_params",
        &TaskCancelParams {
            task_id: TaskId::new(7),
        },
    );
    check_one(fixtures, "task_cancel_result", &TaskCancelResult {});
}

/// `fs.search` + `search.hits` (0.18.0, M4 live search).
fn check_methods_search(fixtures: &BTreeMap<String, Value>) {
    check_one(
        fixtures,
        "fs_search_params",
        &FsSearchParams {
            root: vpath("file:///home/user"),
            name_glob: Some("*.rs".to_owned()),
            name_regex: Some("^ma.n\\.rs$".to_owned()),
            content: Some("año".to_owned()),
            content_regex: Some("a.o".to_owned()),
            case_sensitive: true,
            max_hits: Some(100),
        },
    );
    check_one(
        fixtures,
        "fs_search_params_minimo",
        &FsSearchParams {
            root: vpath("file:///home/user"),
            name_glob: None,
            name_regex: None,
            content: None,
            content_regex: None,
            case_sensitive: false,
            max_hits: None,
        },
    );
    check_one(
        fixtures,
        "search_hits",
        &SearchHits {
            task_id: TaskId::new(7),
            entries: vec![Entry {
                attrs: std::collections::BTreeMap::new(),
                path: vpath("file:///home/user/doc.txt"),
                kind: EntryKind::File,
                size: Some(1234),
                mtime_ms: Some(1_720_000_000_000),
            }],
            matches: Some(vec![MatchInfo {
                line: Some(3),
                preview: Some("hay un año aquí".to_owned()),
            }]),
        },
    );
    check_one(
        fixtures,
        "match_info",
        &MatchInfo {
            line: Some(3),
            preview: Some("hay un año aquí".to_owned()),
        },
    );
}

/// fs.copy/fs.move (con resume/verify de 0.6.0, ADR 0012).
fn check_methods_transfer(fixtures: &BTreeMap<String, Value>) {
    check_one(
        fixtures,
        "fs_copy_params",
        &FsCopyParams {
            from: vpath("file:///src/a.txt"),
            to: vpath("file:///dst/a.txt"),
            on_collision: CollisionPolicy::Fail,
            symlinks: SymlinkPolicy::Preserve,
            resume: ResumePolicy::Off,
            verify: VerifyPolicy::Length,
        },
    );
    check_one(
        fixtures,
        "fs_copy_params_policies",
        &FsCopyParams {
            from: vpath("file:///src/a.txt"),
            to: vpath("file:///dst/a.txt"),
            on_collision: CollisionPolicy::RenameAuto,
            symlinks: SymlinkPolicy::Skip,
            resume: ResumePolicy::On,
            verify: VerifyPolicy::Hash,
        },
    );
    check_one(
        fixtures,
        "fs_move_params",
        &FsMoveParams {
            from: vpath("file:///src/dir"),
            to: vpath("sftp://nas:22/backup/dir"),
            on_collision: CollisionPolicy::Fail,
            symlinks: SymlinkPolicy::Preserve,
            resume: ResumePolicy::Off,
            verify: VerifyPolicy::Length,
        },
    );
}

/// Métodos del daemon (ADR 0011): initialize y daemon.shutdown.
fn check_methods_daemon(fixtures: &BTreeMap<String, Value>) {
    check_one(
        fixtures,
        "initialize_params",
        &InitializeParams {
            client_info: ClientInfo {
                name: "norte-tui".into(),
                version: "0.1.0".into(),
            },
            protocol_version: "0.4.0".into(),
            encodings: vec!["json".into()],
            agent_session: None,
        },
    );
    check_one(
        fixtures,
        "initialize_result",
        &InitializeResult {
            server_info: ServerInfo {
                name: "norte-core".into(),
                version: "0.1.0".into(),
            },
            protocol_version: "0.4.0".into(),
            encodings: vec!["json".into()],
        },
    );
    check_one(
        fixtures,
        "daemon_shutdown_params_graceful",
        &DaemonShutdownParams { graceful: true },
    );
    check_one(
        fixtures,
        "daemon_shutdown_params_hard",
        &DaemonShutdownParams { graceful: false },
    );
    check_one(fixtures, "daemon_shutdown_result", &DaemonShutdownResult {});
}

/// Métodos de 0.5.0 (fase 3): task.list, fs.read, fs.capabilities.
fn check_methods_v05(fixtures: &BTreeMap<String, Value>) {
    check_one(fixtures, "task_list_params", &TaskListParams {});
    check_one(
        fixtures,
        "task_list_result",
        &TaskListResult {
            tasks: vec![TaskProgress {
                task_id: TaskId::new(7),
                kind: TaskKind::Copy,
                state: TaskState::Running,
                bytes_done: 512,
                bytes_total: Some(1024),
                entries_done: 1,
                entries_total: Some(3),
                current: Some(vpath("file:///src/a.txt")),
            }],
        },
    );
    check_one(
        fixtures,
        "fs_read_params",
        &FsReadParams {
            path: vpath("file:///home/user/doc.txt"),
            range: Some(ByteRange {
                offset: 0,
                len: Some(4096),
            }),
        },
    );
    check_one(
        fixtures,
        "fs_read_params_sin_rango",
        &FsReadParams {
            path: vpath("file:///home/user/doc.txt"),
            range: None,
        },
    );
    check_one(
        fixtures,
        "fs_read_result",
        &FsReadResult {
            content_b64: "aG9sYQ==".into(),
            eof: true,
        },
    );
    check_one(
        fixtures,
        "fs_read_result_parcial",
        &FsReadResult {
            content_b64: "MDEy".into(),
            eof: false,
        },
    );
    check_one(
        fixtures,
        "task_list_result_vacio",
        &TaskListResult { tasks: vec![] },
    );
    check_one(
        fixtures,
        "fs_capabilities_params",
        &FsCapabilitiesParams {
            path: vpath("file:///home"),
        },
    );
    check_one(
        fixtures,
        "fs_capabilities_result",
        &FsCapabilitiesResult {
            capabilities: Capabilities {
                flags: CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::SYMLINKS,
                max_path: None,
            },
            attrs: AttrCatalog::default(),
        },
    );
    check_one(
        fixtures,
        "fs_capabilities_result_con_attrs",
        &FsCapabilitiesResult {
            capabilities: Capabilities {
                flags: CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::SYMLINKS,
                max_path: None,
            },
            attrs: AttrCatalog::new(vec![
                AttrInfo {
                    id: "posix.mode".to_owned(),
                    label: "Mode".to_owned(),
                    ty: AttrType::Uint,
                    hint: AttrHint::Mode,
                },
                AttrInfo {
                    id: "sftp.owner".to_owned(),
                    label: "Owner".to_owned(),
                    ty: AttrType::Bytes,
                    hint: AttrHint::Identity,
                },
            ]),
        },
    );
}

/// El envelope JSON-RPC congelado (ADR 0011): la forma de request/response/
/// notification y el objeto de error con la taxonomía en `data`.
#[test]
fn golden_envelope() {
    use norte_proto::wire::{
        JsonRpcVersion, Notification, Request, RequestId, Response, RpcError, codes,
    };
    let fixtures = load("envelope.json");
    check_one(
        &fixtures,
        "request",
        &Request {
            jsonrpc: JsonRpcVersion,
            id: RequestId::Num(7),
            method: "fs.list".into(),
            params: Some(serde_json::json!({"path": "file:///home"})),
        },
    );
    check_one(
        &fixtures,
        "request_null_params",
        &Request {
            jsonrpc: JsonRpcVersion,
            id: RequestId::Num(8),
            method: "daemon.shutdown".into(),
            params: None,
        },
    );
    check_one(
        &fixtures,
        "notification",
        &Notification {
            jsonrpc: JsonRpcVersion,
            method: "task.progress".into(),
            params: Some(serde_json::json!({"task_id": 3})),
        },
    );
    check_one(
        &fixtures,
        "response_ok",
        &Response::ok(RequestId::Num(7), serde_json::json!({"entries": []})),
    );
    check_one(
        &fixtures,
        "response_app_error",
        &Response::err(Some(RequestId::Num(7)), RpcError::from(Error::NotFound)),
    );
    check_one(
        &fixtures,
        "response_protocol_error_null_id",
        &Response::err(None, RpcError::protocol(codes::PARSE_ERROR, "invalid JSON")),
    );
    check_one(
        &fixtures,
        "request_string_id",
        &Request {
            jsonrpc: JsonRpcVersion,
            id: RequestId::Str("req-abc".into()),
            method: "fs.stat".into(),
            params: Some(serde_json::json!({"path": "file:///x"})),
        },
    );
    assert_eq!(fixtures.len(), 7, "[envelope.json] fixtures sin caso Rust");
}

/// Los CÓDIGOS JSON-RPC y el límite de frame son wire observable: un typo
/// no puede pasar CI (hallazgo m1 del protocol-guardian).
#[test]
fn rpc_codes_y_limites_congelados() {
    use norte_proto::wire::{MAX_FRAME_BYTES, codes};
    assert_eq!(codes::PARSE_ERROR, -32700);
    assert_eq!(codes::INVALID_REQUEST, -32600);
    assert_eq!(codes::METHOD_NOT_FOUND, -32601);
    assert_eq!(codes::INVALID_PARAMS, -32602);
    assert_eq!(codes::INTERNAL_ERROR, -32603);
    assert_eq!(codes::APP_ERROR, -32000);
    assert_eq!(codes::VERSION_MISMATCH, -32001);
    assert_eq!(codes::NOT_INITIALIZED, -32002);
    assert_eq!(codes::OVERLOADED, -32003);
    assert_eq!(MAX_FRAME_BYTES, 16 * 1024 * 1024);
}

#[test]
fn method_names_frozen() {
    use norte_proto::methods;
    assert_eq!(methods::FS_LIST, "fs.list");
    assert_eq!(methods::FS_STAT, "fs.stat");
    assert_eq!(methods::FS_COPY, "fs.copy");
    assert_eq!(methods::FS_MOVE, "fs.move");
    assert_eq!(methods::FS_DELETE, "fs.delete");
    assert_eq!(methods::TASK_CANCEL, "task.cancel");
    assert_eq!(methods::TASK_PROGRESS, "task.progress");
    assert_eq!(methods::INITIALIZE, "initialize");
    assert_eq!(methods::DAEMON_SHUTDOWN, "daemon.shutdown");
    assert_eq!(methods::TASK_LIST, "task.list");
    assert_eq!(methods::FS_READ, "fs.read");
    assert_eq!(methods::FS_CAPABILITIES, "fs.capabilities");
    assert_eq!(
        methods::CONNECTION_TRUST_HOST_KEY,
        "connection.trust_host_key"
    );
    // Familia policy.* (0.11.0/0.12.0): gobernanza de agentes. El pin llegó
    // con retraso (MINOR-1 del protocol-guardian en el bump 0.12).
    assert_eq!(methods::POLICY_REQUEST_SCOPE, "policy.request_scope");
    assert_eq!(methods::POLICY_GRANT_SCOPE, "policy.grant_scope");
    assert_eq!(methods::POLICY_DECIDE, "policy.decide");
    assert_eq!(methods::POLICY_PENDING, "policy.pending");
    assert_eq!(
        methods::POLICY_APPROVAL_REQUIRED,
        "policy.approval_required"
    );
    assert_eq!(methods::POLICY_UNDO_SESSION, "policy.undo_session");
    assert_eq!(methods::POLICY_UNDO_REPORT, "policy.undo_report");
    // Familia plugin.* (0.13.0, M4-P3): catálogo + gobernanza humana.
    assert_eq!(methods::PLUGIN_LIST, "plugin.list");
    assert_eq!(methods::PLUGIN_SET_APPROVAL, "plugin.set_approval");
    assert_eq!(methods::PLUGIN_SET_ENABLED, "plugin.set_enabled");
    assert_eq!(methods::PLUGIN_RUN_COMMAND, "plugin.run_command");
    assert_eq!(methods::PLUGIN_PREVIEW, "plugin.preview");
    // Familia plugin.* de datos ESTRUCTURADOS v2 (0.27.0, G3, ADR 0037): el
    // host pinta, nunca el plugin. Aditivo sobre 0.26.x.
    assert_eq!(methods::PLUGIN_PREVIEW_STYLED, "plugin.preview_styled");
    assert_eq!(methods::PLUGIN_DECORATE, "plugin.decorate");
    assert_eq!(methods::PLUGIN_COLUMN_VALUES, "plugin.column_values");
    assert_eq!(methods::FS_READ_MAX_CHUNK, 8 * 1024 * 1024);
    assert_eq!(methods::FS_LIST_MAX_PAGE, 10_000);
    // 0.34.0 (H3e): el tope de `PluginHelpResult::markdown`. El LITERAL, no el
    // símbolo: el contrato invita a un receptor a dimensionar contra él, así
    // que cambiarlo es cambiar el wire y tiene que ponerse algo rojo.
    // `norte-core` ancla aparte que este número y el del host son el mismo.
    assert_eq!(methods::PLUGIN_HELP_MAX_BYTES, 64 * 1024);
    // 0.18.0 (M4 live search): fs.search + search.hits + TaskKind::Search.
    // Aditivo sobre 0.17.x.
    assert_eq!(methods::FS_SEARCH, "fs.search");
    assert_eq!(methods::SEARCH_HITS, "search.hits");
    assert_eq!(methods::SEARCH_HITS_MAX_BATCH, 256);
    // 0.19.0 (#72): rpc.cancel { id }. Aditivo sobre 0.18.x.
    assert_eq!(methods::RPC_CANCEL, "rpc.cancel");
    // 0.20.0 (#44): connection.degraded (server→client). Aditivo sobre 0.19.x.
    assert_eq!(methods::CONNECTION_DEGRADED, "connection.degraded");
    // 0.21.0 (#55, ADR 0028): tar+gz en ARCHIVE_FORMATS (longest-match). No
    // añade método/notificación nueva — el bump señala la capacidad de
    // interpretar schemes `tar+gz+…`.
    assert!(norte_proto::ARCHIVE_FORMATS.contains(&"tar+gz"));
    // 0.22.0 (#93): campo opcional `skipped` en FsListResult. No añade
    // método/notificación — el bump señala el metadato aditivo del listado.
    // 0.23.0 (#95): variante Error::LimitExceeded{limit} — límite local ≠
    // Corrupt. Vocabulario CERRADO pineado aquí: SOLO las dos constantes
    // (cd-bytes NO existe — max_cd_bytes solo gatea el cacheo del CD).
    assert_eq!(norte_proto::Error::LIMIT_ENTRIES, "entries");
    assert_eq!(
        norte_proto::Error::LIMIT_DECOMPRESSED_BYTES,
        "decompressed-bytes"
    );
    // 0.24.0 (#56): direccionamiento multi-capa + tope de anidamiento en el
    // vocabulario de LimitExceeded (tres constantes, sigue CERRADO).
    assert_eq!(norte_proto::Error::LIMIT_NESTING, "nesting");
    // 0.25.0 (M4, ADR 0034): índice de búsqueda. index.build (Task) + index.query.
    assert_eq!(methods::INDEX_BUILD, "index.build");
    assert_eq!(methods::INDEX_QUERY, "index.query");
    // 0.26.0 (P1): PluginInfo gana description + commands (sin método nuevo).
    // 0.27.0 (G3, ADR 0037): plugin.preview_styled/decorate/column_values —
    // datos estructurados de plugin, pinta el host.
    assert_eq!(methods::PLUGIN_GET_CONFIG, "plugin.get_config");
    assert_eq!(methods::PLUGIN_SET_CONFIG, "plugin.set_config");
    // 0.28.0 (G3c, ADR 0037): plugin.get_config/set_config — [config] de P2
    // por el wire; PluginInfo gana columns (sin método nuevo).
    // 0.29.0 (#101): PluginPreview/PluginPreviewStyled ganan `lossy` (sin
    // método nuevo — solo campo aditivo).
    // 0.30.0 (columnas bloque 1, ADR 0039): atributos de provider — Entry.attrs,
    // FsCapabilitiesResult.attrs y los dos attrs de petición (sin método nuevo).
    // 0.31.0 (#104): fs.mkdir (Task) + TaskKind::Mkdir. Aditivo sobre 0.30.x.
    assert_eq!(methods::FS_MKDIR, "fs.mkdir");
    // 0.32.0 (M4-IA, ADR 0031): ai.rename_plan — respuesta directa cancelable.
    assert_eq!(methods::AI_RENAME_PLAN, "ai.rename_plan");
    // 0.33.0 (M4-IA-2, ADR 0031 A3): index.embed (Task, TaskKind::Embed) +
    // index.search_semantic (directa cancelable, k recortado al tope).
    assert_eq!(methods::INDEX_EMBED, "index.embed");
    assert_eq!(methods::INDEX_SEARCH_SEMANTIC, "index.search_semantic");
    assert_eq!(methods::INDEX_SEMANTIC_MAX_K, 100);
    // 0.34.0 (H3e): plugin.help — la página de ayuda de UN plugin bajo
    // demanda; PluginInfo gana has_help (discovery barato, sin método nuevo).
    assert_eq!(methods::PLUGIN_HELP, "plugin.help");
    // 0.35.0 (#120): PluginColumnValuesParams gana `plugin_id` — sin método
    // nuevo, así que aquí solo se mueve la versión.
    // 0.36.0 (batch rename): las DOS mitades del ejecutor — el plan revisable
    // (respuesta directa) y su ejecución (Task, `TaskKind::RenameBatch`),
    // ligadas por el `plan_hash` que el humano aprobó.
    assert_eq!(methods::FS_RENAME_BATCH_PLAN, "fs.rename_batch_plan");
    assert_eq!(methods::FS_RENAME_BATCH, "fs.rename_batch");
    // Los LITERALES, no los símbolos, por el mismo motivo que
    // `PLUGIN_HELP_MAX_BYTES` arriba: un receptor dimensiona contra ellos —
    // rechaza el lote antes de mandarlo, reserva el buffer del hash — así que
    // moverlos mueve el contrato y algo tiene que ponerse rojo. El tope de
    // parejas RECHAZA (no recorta como `FS_LIST_MAX_PAGE`), y por eso importa
    // aún más que un tercero lo conozca.
    assert_eq!(methods::FS_RENAME_BATCH_MAX_PAIRS, 4096);
    assert_eq!(methods::PLAN_HASH_LEN, 64);
    assert_eq!(norte_proto::PROTOCOL_VERSION, "0.36.0");
}

#[test]
fn golden_transfer() {
    check_family(
        "transfer.json",
        &[
            (
                "byte_range_full",
                ByteRange {
                    offset: 0,
                    len: None,
                },
            ),
            (
                "byte_range_chunk",
                ByteRange {
                    offset: 65536,
                    len: Some(1_048_576),
                },
            ),
        ],
    );
    check_family(
        "transfer_collision.json",
        &[
            ("fail", CollisionPolicy::Fail),
            ("ask", CollisionPolicy::Ask),
            ("skip", CollisionPolicy::Skip),
            ("overwrite", CollisionPolicy::Overwrite),
            ("rename_auto", CollisionPolicy::RenameAuto),
            ("newer", CollisionPolicy::Newer),
        ],
    );
    check_family(
        "transfer_delete_mode.json",
        &[
            ("trash", norte_proto::DeleteMode::Trash),
            ("permanent", norte_proto::DeleteMode::Permanent),
        ],
    );
    check_family(
        "transfer_symlinks.json",
        &[
            ("follow", SymlinkPolicy::Follow),
            ("preserve", SymlinkPolicy::Preserve),
            ("skip", SymlinkPolicy::Skip),
        ],
    );
    check_family(
        "transfer_resume.json",
        &[("off", ResumePolicy::Off), ("on", ResumePolicy::On)],
    );
    check_family(
        "transfer_verify.json",
        &[
            ("length", VerifyPolicy::Length),
            ("hash", VerifyPolicy::Hash),
        ],
    );
}

#[test]
fn golden_attrs() {
    check_family(
        "attr_type.json",
        &[
            ("uint", AttrType::Uint),
            ("int", AttrType::Int),
            ("text", AttrType::Text),
            ("bytes", AttrType::Bytes),
            ("time_ms", AttrType::TimeMs),
            ("bool", AttrType::Bool),
            ("unknown", AttrType::Unknown),
        ],
    );
    check_family(
        "attr_hint.json",
        &[
            ("size", AttrHint::Size),
            ("timestamp", AttrHint::Timestamp),
            ("mode", AttrHint::Mode),
            ("identity", AttrHint::Identity),
            ("opaque", AttrHint::Opaque),
            ("unknown", AttrHint::Unknown),
        ],
    );
    check_family(
        "attr_info.json",
        &[(
            "posix_mode",
            AttrInfo {
                id: "posix.mode".to_owned(),
                label: "Mode".to_owned(),
                ty: AttrType::Uint,
                hint: AttrHint::Mode,
            },
        )],
    );
    let attr_values = [
        ("uint", AttrValue::Uint(33188)),
        // u64::MAX: donde un cliente JS pierde el valor en su f64.
        ("uint_max", AttrValue::Uint(u64::MAX)),
        ("int", AttrValue::Int(-7)),
        ("text", AttrValue::Text("STANDARD_IA".to_owned())),
        // Bytes que NO son UTF-8: la razón de existir de la variante.
        ("bytes_b64", AttrValue::Bytes(vec![0xFF, 0xFE])),
        // Vacío NO es ausente: la celda existe y su valor son cero bytes.
        ("bytes_b64_empty", AttrValue::Bytes(Vec::new())),
        // Negativo: pre-1970 es real y el wire lo admite.
        ("time_ms", AttrValue::TimeMs(-86_400_000)),
        ("bool", AttrValue::Bool(true)),
        ("unknown", AttrValue::Unknown),
    ];
    // Exhaustividad: `attr_value_tag` es un `match` sin comodín, así que una
    // variante NUEVA rompe la compilación hasta que alguien la cubra; este
    // set-check convierte "añadí la variante, olvidé la fixture" en rojo.
    let cubiertas: BTreeSet<&str> = attr_values.iter().map(|(_, v)| attr_value_tag(v)).collect();
    let todas: BTreeSet<&str> = ATTR_VALUE_TAGS.into_iter().collect();
    assert_eq!(
        cubiertas, todas,
        "[attr_value.json] toda variante de AttrValue necesita al menos una fixture"
    );
    check_family("attr_value.json", &attr_values);
}

/// Todas las etiquetas de wire de [`AttrValue`], cruzadas contra el `match`
/// exhaustivo de [`attr_value_tag`].
const ATTR_VALUE_TAGS: [&str; 7] = [
    "uint",
    "int",
    "text",
    "bytes_b64",
    "time_ms",
    "bool",
    "unknown",
];

/// Etiqueta de wire de un valor. EXHAUSTIVO por construcción (sin `_`): añadir
/// una variante a `AttrValue` rompe aquí la compilación.
fn attr_value_tag(v: &AttrValue) -> &'static str {
    match v {
        AttrValue::Uint(_) => "uint",
        AttrValue::Int(_) => "int",
        AttrValue::Text(_) => "text",
        AttrValue::Bytes(_) => "bytes_b64",
        AttrValue::TimeMs(_) => "time_ms",
        AttrValue::Bool(_) => "bool",
        AttrValue::Unknown => "unknown",
    }
}
