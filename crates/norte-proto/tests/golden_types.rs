//! Golden tests de los tipos del protocolo (spec §12): cada fixture JSON es
//! el wire format congelado. Match estructural exacto (`serde_json::Value`)
//! bidireccional — el orden de claves y el formato de whitespace NO son parte
//! del contrato JSON-RPC; nombres, tipos y valores sí. Romper uno de estos
//! tests = cambio de wire format = bump de versión + revisión doble.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::path::Path;

use norte_proto::methods::{
    ClientInfo, DaemonGoingAway, DaemonShutdownParams, DaemonShutdownResult, FsCapabilitiesParams,
    FsCapabilitiesResult, FsCopyParams, FsDeleteParams, FsListParams, FsListResult, FsMoveParams,
    FsReadParams, FsReadResult, FsSearchParams, FsStatParams, FsStatResult, FsTaskResult,
    InitializeParams, InitializeResult, MatchInfo, SearchHits, ServerInfo, ShutdownMode,
    TaskCancelParams, TaskCancelResult, TaskListParams, TaskListResult,
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
/// Una ruta RELATIVA a las raíces de un plan (0.40.0) desde su forma wire.
fn rel_path(wire: &str) -> norte_proto::methods::RelPath {
    norte_proto::methods::RelPath::parse_wire(wire).expect("rel de fixture")
}

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

// La tabla CONGELADA de la taxonomía entera. Trocearla por longitud
// escondería justo lo que `check_family` comprueba —cobertura 1:1 entre
// fixture y variante—, así que aquí la longitud es la propiedad.
#[expect(
    clippy::too_many_lines,
    reason = "una aserción por fixture y variante: la longitud es la propiedad"
)]
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
            // El tope de la sesión de UI (0.48.0, L2). Congelado como los
            // otros dos: el token es lo ÚNICO que distingue «recorta el
            // historial» de «hay demasiadas entradas», y es un string.
            (
                "limit_exceeded_session_body",
                Error::LimitExceeded {
                    limit: Error::LIMIT_SESSION_BODY.into(),
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
                "conflict_escapes_root",
                Error::Conflict {
                    conflict: ConflictKind::EscapesRoot,
                },
            ),
            (
                "conflict_stale_revision",
                Error::Conflict {
                    conflict: ConflictKind::StaleRevision,
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
            // Las TRES del vocabulario cerrado de #279: van una a una porque
            // lo que este golden congela es el vocabulario, y una sola fixture
            // dejaría que las otras dos cambiaran de nombre sin que nada lo
            // notara.
            (
                "approval_gone_unknown",
                Error::ApprovalGone {
                    reason: "unknown".to_owned(),
                },
            ),
            (
                "approval_gone_expired",
                Error::ApprovalGone {
                    reason: "expired".to_owned(),
                },
            ),
            (
                "approval_gone_already_decided",
                Error::ApprovalGone {
                    reason: "already-decided".to_owned(),
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
            // 0.63.0 (#325): la conexión pide un secreto que no está en
            // ninguna parte. Fixture propia porque es una categoría más de la
            // familia «esto no se puede seguir sin un humano», y con una sola
            // de la familia las demás se podrían renombrar sin que nada lo
            // notara.
            (
                "secret_needed",
                // El `endpoint` va en la fixtura porque es lo que hace
                // contestable el diálogo, y sin él nada impediría que alguien
                // lo quitara «porque el nombre ya está» (#325).
                Error::SecretNeeded {
                    conn: "rosetta".to_owned(),
                    endpoint: "s3://s3.eu-west-1.amazonaws.com".to_owned(),
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
            // 0.40.0 (sincronización): las dos raíces son el mismo árbol. LAS
            // TRES relaciones, porque `relation` es lo único que la variante
            // dice y porque «son la misma» NO es un caso degenerado de «una
            // está dentro de la otra»: es la frase que el frontend pinta.
            (
                "overlapping_roots_same",
                Error::OverlappingRoots {
                    relation: norte_proto::RootOverlap::Same,
                },
            ),
            (
                "overlapping_roots_source_inside_dest",
                Error::OverlappingRoots {
                    relation: norte_proto::RootOverlap::SourceInsideDest,
                },
            ),
            (
                "overlapping_roots_dest_inside_source",
                Error::OverlappingRoots {
                    relation: norte_proto::RootOverlap::DestInsideSource,
                },
            ),
            // 0.41.0 (#178): el journal de esta sesión no se puede abrir y la
            // mutación se rehúsa. Sin campos, y eso es la mitad del contrato:
            // la ruta del fichero y el error de `SQLite` son locales del
            // proceso que la emite y no cruzan la frontera.
            ("journal_unavailable", Error::JournalUnavailable),
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

// Una fixture por `TaskKind` que el core emite, con la SEMÁNTICA de progreso
// de cada uno escrita al lado. Es una tabla: trocearla por longitud escondería
// que la cobertura es una por clase.
#[expect(
    clippy::too_many_lines,
    reason = "una aserción por clase de progreso: la longitud es la cobertura"
)]
#[test]
fn golden_task_progress() {
    check_family(
        "task_progress.json",
        &[
            (
                // 0.53.0 (#251): un `fs.dir_size` que terminó habiendo dejado
                // subárboles sin leer. Es el ÚNICO caso que congela el nombre
                // `unreadable` y su forma: los demás lo llevan a `None` y por
                // tanto no lo emiten, así que sin éste renombrarlo o anidarlo
                // no pondría rojo nada.
                //
                // Y `Some(0)` no es `None`: «los conté y no hubo» es una
                // respuesta, «no los cuento» es otra, y confundirlas es lo que
                // hace que un cliente pinte un total corto con cara de seguro.
                "dir_size_con_ilegibles",
                TaskProgress {
                    task_id: TaskId::new(21),
                    kind: TaskKind::DirSize,
                    state: TaskState::Completed,
                    bytes_done: 4096,
                    bytes_total: Some(4096),
                    entries_done: 12,
                    entries_total: Some(12),
                    current: None,
                    unreadable: Some(3),
                    unvisited: None,
                },
            ),
            (
                "dir_size_todo_legible",
                TaskProgress {
                    task_id: TaskId::new(22),
                    kind: TaskKind::DirSize,
                    state: TaskState::Completed,
                    bytes_done: 4096,
                    bytes_total: Some(4096),
                    entries_done: 12,
                    entries_total: Some(12),
                    current: None,
                    unreadable: Some(0),
                    unvisited: None,
                },
            ),
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
                    unreadable: None,
                    unvisited: None,
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
                    unreadable: None,
                    unvisited: None,
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
                    unreadable: None,
                    unvisited: None,
                },
            ),
            (
                // 0.60.0 (#314): TaskKind::SetMode. Su progreso cuenta
                // ENTRADAS y NO bytes —un `chmod` no mueve ninguno—, y el
                // total se sabe desde el principio porque son las rutas que se
                // mandaron. Congelar esa forma es lo que impide que alguien
                // pinte una barra de bytes que se quedaría en cero.
                "running_set_mode",
                TaskProgress {
                    task_id: TaskId::new(58),
                    kind: TaskKind::SetMode,
                    state: TaskState::Running,
                    bytes_done: 0,
                    bytes_total: None,
                    entries_done: 1,
                    entries_total: Some(2),
                    current: Some(vpath("file:///casa/b%FF.bin")),
                    unreadable: None,
                    unvisited: None,
                },
            ),
            (
                // 0.59.0 (#311): TaskKind::Checksum, y la FORMA de su progreso,
                // que la rustdoc promete y hasta ahora no congelaba nada: hay
                // total de entradas desde el principio —se sabe cuántas rutas
                // se pidieron— y `bytes_total` es `None`, porque cuánto ocupan
                // no se sabe sin haberlas leído. `unreadable` cuenta las que se
                // quedaron sin digest (#251).
                "running_checksum",
                TaskProgress {
                    task_id: TaskId::new(57),
                    kind: TaskKind::Checksum,
                    state: TaskState::Running,
                    bytes_done: 4096,
                    bytes_total: None,
                    entries_done: 2,
                    entries_total: Some(4),
                    current: Some(vpath("file:///casa/b%FF.bin")),
                    unreadable: Some(1),
                    unvisited: None,
                },
            ),
            (
                // 0.49.0 (#139): TaskKind::DirSize, y la fixture que no se
                // escribió cuando entró el método (hallazgo de
                // `protocol-guardian`). Su progreso es el ÚNICO cuyo
                // `bytes_done` ES el resultado — no hay tipo de result—, y por
                // eso los totales van a `None` hasta el snapshot terminal: una
                // barra hacia un número inventado sería peor que ninguna.
                "running_dir_size",
                TaskProgress {
                    task_id: TaskId::new(31),
                    kind: TaskKind::DirSize,
                    state: TaskState::Running,
                    bytes_done: 4096,
                    bytes_total: None,
                    entries_done: 12,
                    entries_total: None,
                    current: Some(vpath("file:///home/user/proj/src")),
                    unreadable: None,
                    unvisited: None,
                },
            ),
            (
                // 0.50.0 (#132): TaskKind::Pack. `bytes_*` cuenta lo LEÍDO del
                // origen, no lo escrito: cuánto va a ocupar el archivo lo
                // decide el compresor, y prometer ese total sería prometer un
                // número que va a fallar. `entries_*` sí tiene total, porque
                // las entradas se enumeran antes de empezar.
                "running_pack",
                TaskProgress {
                    task_id: TaskId::new(32),
                    kind: TaskKind::Pack,
                    state: TaskState::Running,
                    bytes_done: 2048,
                    bytes_total: Some(8192),
                    entries_done: 2,
                    entries_total: Some(5),
                    current: Some(vpath("file:///proj/src/main.rs")),
                    unreadable: None,
                    unvisited: None,
                },
            ),
            (
                // 0.50.0 (#132): TaskKind::TestArchive. Se conocen las entradas
                // (están en el índice) pero no cuántos bytes hay que leer hasta
                // haberlos leído — un zip declara tamaños que el test existe
                // justo para no creerse.
                "running_test_archive",
                TaskProgress {
                    task_id: TaskId::new(33),
                    kind: TaskKind::TestArchive,
                    state: TaskState::Running,
                    bytes_done: 1024,
                    bytes_total: None,
                    entries_done: 4,
                    entries_total: Some(9),
                    current: Some(vpath("file:///a.zip")),
                    unreadable: None,
                    unvisited: None,
                },
            ),
            (
                // 0.50.0 (#132): TaskKind::Split. Los dos totales se saben
                // desde el principio —el tamaño del fichero y la división
                // entera—, así que es de las pocas barras honestas de punta a
                // punta. `current` es el TROZO que se está escribiendo.
                "running_split",
                TaskProgress {
                    task_id: TaskId::new(34),
                    kind: TaskKind::Split,
                    state: TaskState::Running,
                    bytes_done: 1_048_576,
                    bytes_total: Some(3_145_728),
                    entries_done: 1,
                    entries_total: Some(3),
                    current: Some(vpath("file:///trozos/g.iso.002")),
                    unreadable: None,
                    unvisited: None,
                },
            ),
            (
                // 0.50.0 (#132): TaskKind::Combine, el reverso: los trozos se
                // enumeran y se miden ANTES de escribir nada —es lo que permite
                // rechazar un hueco sin haber creado el destino—, así que
                // también lleva los dos totales.
                "running_combine",
                TaskProgress {
                    task_id: TaskId::new(35),
                    kind: TaskKind::Combine,
                    state: TaskState::Running,
                    bytes_done: 2_097_152,
                    bytes_total: Some(3_145_728),
                    entries_done: 2,
                    entries_total: Some(3),
                    current: Some(vpath("file:///g.iso")),
                    unreadable: None,
                    unvisited: None,
                },
            ),
            (
                // 0.39.0 (ADR 0048): TaskKind::Compare en el wire, y con él la
                // SEMÁNTICA del progreso de una comparación — `entries_*`
                // cuenta PAREJAS emitidas, y `bytes_*` es cero/`None` porque
                // con el rung de hash apagado no se lee un solo byte. El total
                // es `None` a propósito: el walk no sabe cuántas parejas hay
                // hasta que termina de recorrer los dos árboles.
                "running_compare",
                TaskProgress {
                    task_id: TaskId::new(17),
                    kind: TaskKind::Compare,
                    state: TaskState::Running,
                    bytes_done: 0,
                    bytes_total: None,
                    entries_done: 120,
                    entries_total: None,
                    current: Some(vpath("file:///home/user/origen/fotos")),
                    unreadable: None,
                    unvisited: None,
                },
            ),
            (
                // 0.40.0 (ADR 0049): TaskKind::SyncPlan en el wire, y con él la
                // SEMÁNTICA de su progreso — `entries_*` cuenta PASOS emitidos
                // y `bytes_*` es cero/`None`, exactamente como en `Compare`:
                // planificar no escribe un byte, y con el rung de hash apagado
                // tampoco lee ninguno.
                "running_sync_plan",
                TaskProgress {
                    task_id: TaskId::new(19),
                    kind: TaskKind::SyncPlan,
                    state: TaskState::Running,
                    bytes_done: 0,
                    bytes_total: None,
                    entries_done: 120,
                    entries_total: None,
                    current: Some(vpath("file:///home/user/origen/fotos")),
                    unreadable: None,
                    unvisited: None,
                },
            ),
            (
                // 0.40.0 (ADR 0049): TaskKind::Sync, la OTRA mitad y la que sí
                // mueve bytes. Es el contraste que hace legible al de arriba:
                // misma familia, `bytes_*` poblado, porque aquí sí se copia.
                // Los dos tokens llegan a un cliente 0.39 SIN que haya llamado
                // a nada —`task.progress` se difunde a toda conexión humana—,
                // así que congelar su ortografía es congelar la única
                // superficie N/N-1 de este bump.
                "running_sync",
                TaskProgress {
                    task_id: TaskId::new(20),
                    kind: TaskKind::Sync,
                    state: TaskState::Running,
                    bytes_done: 4096,
                    bytes_total: Some(65536),
                    entries_done: 3,
                    entries_total: Some(40),
                    current: Some(vpath("file:///home/user/copia/informe%FF%FE.dat")),
                    unreadable: None,
                    unvisited: None,
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
                    unreadable: None,
                    unvisited: None,
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
                    unreadable: None,
                    unvisited: None,
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
                    unreadable: None,
                    unvisited: None,
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
    check_methods_ui_session(&fixtures);
    check_methods_dir_size(&fixtures);
    check_methods_archive_write(&fixtures);
    check_methods_plugin(&fixtures);
    check_methods_rpc(&fixtures);
    check_methods_index(&fixtures);
    check_methods_ai(&fixtures);
    check_methods_rename_batch(&fixtures);
    check_methods_host(&fixtures);
    check_methods_compare(&fixtures);
    check_methods_sync(&fixtures);
    check_methods_sync_notifs(&fixtures);
    check_methods_sync_apply(&fixtures);
    check_methods_log(&fixtures);
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
    // familia pinea varias formas de plan). 116 → 120: + fs_rename_batch_report_params y
    // fs_rename_batch_report_result(/_clean/_uncertain) — el informe del lote:
    // limpio, atascado, y con el paso de destino desconocido.
    // 120 → 126 en 0.37.0 (#131): + host_volumes_params(/_pseudo),
    // host_volumes_result y los tipos sueltos volume/volume_hostile_no_sizes/
    // volume_future_kind (la forma "sin sizes" y el degrade `serde(other)`).
    // 126 → 130 en 0.39.0 (ADR 0048): + fs_compare_params(/_minimo) y
    // compare_rows_batch(/_empty) — la petición con todo poblado y la MÍNIMA
    // (que es la que congela los defaults), más el lote y su forma vacía. La
    // FILA tiene fichero propio (`compare_row.json`): su familia pinea una
    // forma por veredicto.
    // 130 → 142 en 0.40.0 (ADR 0049): + sync_plan_params(/_minimo),
    // sync_steps_batch(/_empty), sync_plan_done(/_blocked/_opaque),
    // sync_apply_params, sync_report_params y
    // sync_report_result(/_clean/_died). El PASO y el BLOQUEO tienen fichero
    // propio (`sync_step.json`, `sync_blocker.json`): sus familias pinean una
    // forma por clase. Los TRES cierres de plan son las tres papeleras
    // ([`DestTrash`]), que es lo que decide si el plan se puede deshacer.
    // 142 → 144 en 0.46.0 (roadmap ítem 10): + daemon_shutdown_params_handover
    // y daemon_going_away. El relevo tiene fixture PROPIA en vez de cambiar la
    // de la parada, que es lo que deja ver de un vistazo que el mensaje de una
    // parada corriente no ha cambiado un byte.
    // 144 → 148 en 0.48.0 (L2): + session_get_result, session_get_result_empty,
    // session_put_params y session_put_result. El GET y el PUT llevan la MISMA
    // sesión a propósito: lo que la fixture demuestra es que el cuerpo vuelve
    // igual que fue. La VACÍA tiene fixture propia porque es la que sale en
    // cada primer arranque y la única con `body: null`.
    // 148 → 151 en 0.49.0: + fs_dir_size_params (#139) y los dos de
    // `connection.close` (#140). El RESULT no tiene
    // fixture propia porque no tiene tipo propio — es el `FsTaskResult` de
    // siempre, ya congelado.
    // 151 → 156 en 0.50.0 (#132): + archive_pack_params, archive_test_params,
    // archive_test_result, file_split_params y file_combine_params. Los tres
    // results de pack/split/combine no tienen fixture porque no tienen tipo
    // propio — son el `FsTaskResult` de siempre, ya congelado. Desempaquetar no
    // aparece en absoluto: es un `fs.copy`, y su forma lleva congelada desde
    // 0.10.
    // 156 → 158 al aplicar la revisión: + archive_test_report_params (el
    // QUINTO método del bump, que no estaba congelado en ningún sitio) y
    // archive_test_result_clean (la forma que de verdad devuelve un archivo
    // sano: con todos los campos `serde(default)`, un resultado limpio es `{}`
    // en el wire, y es el que ningún golden fijaba).
    // 158 → 161 en 0.53.0: + plugin_info_with_digest y
    // plugin_set_approval_params_anchored (#282, el ancla que un humano leyó
    // viajando a la ida y a la vuelta) y plugin_list_result_dir_bytes (#265,
    // los bytes del basename). Los tres campos son opcionales, así que sin
    // estas fixturas su NOMBRE y su forma en el wire —el hex de un sha256, el
    // base64 de `label_wire`— no los congelaba nada.
    // 161 → 164 en 0.54.0 (#295): + fs_list_result_anchored,
    // fs_copy_params_anchored y fs_move_params_anchored. Los tres campos son
    // opcionales y se omiten, así que sin estas fixturas ni el NOMBRE del
    // campo ni su forma en el wire —una cadena hex opaca, jamás un inodo—
    // los congelaba nada.
    // 0.57.0 (#290): fs_create_params con su nombre percent-encoded, y su
    // pareja anclada — `fs.create` lleva `dest_anchor` y `fs.mkdir` no, que es
    // lo que hay que congelar.
    // 166 → 169 en 0.58.0 (#250): + archive_pack_report_params y las dos
    // formas del informe. Los tokens de `fold` (`unicode`/`case`/`full`) y de
    // `risk` (`separator`/`stream`/`reserved`/`trailing`) son vocabulario del
    // wire y estas fixturas son lo único que los congela — uno por valor,
    // porque una sola dejaría renombrar los otros sin que nada se enterara. Y
    // la forma LIMPIA va aparte porque significa algo por sí sola: «se
    // comprobaron doce entradas y no había nada», que no es lo mismo que un
    // daemon que no comprueba.
    // 169 → 172 en 0.59.0 (#311): + fs_checksum_params y las dos formas del
    // informe. Los tokens de `miss` (`unreadable`/`not_a_file`) son vocabulario
    // del wire y esta fixtura es lo único que los congela — los dos en la
    // misma, porque van en la misma lista. Y una ruta que NO es UTF-8 entre
    // ellas: el informe tiene que poder nombrar el fichero que no se pudo leer
    // aunque su nombre no sea texto (regla 1).
    // 172 → 173 en 0.60.0 (#314): + fs_set_mode_params, con el modo en su
    // forma NUMÉRICA y una ruta que no es UTF-8.
    // 173 → 175 en 0.62.0 (#315, #121): + fs_set_mode_params_recursivo y
    // ai_rename_plan_params_seleccion. Las dos son fixturas APARTE y no un
    // campo más en las que ya había, porque los tres campos nuevos se OMITEN
    // cuando están vacíos: con una sola fixtura por método, el día que dejaran
    // de omitirse —o que el default de `recursive` cambiara— el wire cambiaría
    // sin que nada se pusiera rojo.
    // 178 → 186 en 0.64.0 (#322): + `connection.failed`, con UNA fixtura POR
    // VALOR de su vocabulario cerrado (siete) más la que NO lleva los dos
    // campos opcionales. Una sola dejaría renombrar los otros seis sin que
    // nada se pusiera rojo, y `reason` se compara por igualdad en el frontend:
    // un renombrado silencioso es una frase que deja de salir.
    // 186 → 196 en 0.65.0 (#328): + los diez de `log.tail`/`log.level`. Cinco
    // de ellos son UNO POR VALOR del vocabulario de niveles: con una sola
    // fixtura se podrían renombrar los otros cuatro sin que nada se pusiera
    // rojo, y `level` se compara por igualdad —para colorear una fila, para
    // marcar cuál está puesto y para decidir qué se captura—, así que un
    // renombrado silencioso es un panel que deja de colorear. Las otras cinco
    // congelan las dos formas que significan algo por sí solas: la petición
    // SIN cursor (que viaja como `null` explícito y quiere decir «lo que
    // tengas», no «desde el principio») y el sondeo que no encontró nada
    // (`lines: []` con `lost: 0`, que es la respuesta más frecuente y la
    // única que distingue «no ha pasado nada» de «se perdió algo»).
    // 196 → 198 en 0.66.0 (D4): + `span_wire_bg` y
    // `plugin_preview_styled_params_columns`. Fixturas APARTE porque los dos
    // campos se omiten cuando faltan: las de antes prueban que el wire viejo
    // no se movió, estas que el nuevo existe.
    // 198 → 200 en 0.67.0 (ADR 0095): + `plugin_command_info_renamer` y
    // `plugin_rename_plan_params`.
    // 200 → 201 en 0.68.0 (#332): + `ai_rename_plan_result_refused`.
    // 201 → 203 en 0.69.0 (ADR 0100): + `plugin_notice_notify` y
    // `plugin_notice_hooks_disabled`, UNA POR VALOR del vocabulario de `kind`:
    // el frontend decide por igualdad si traduce la clase o pinta el texto.
    assert_eq!(fixtures.len(), 203, "[methods.json] fixtures sin caso Rust");
}

/// `log.tail` y `log.level` (0.65.0, #328): el registro del DAEMON.
///
/// Lo que congelan estas fixturas es el vocabulario cerrado de niveles —cinco
/// cadenas comparadas por igualdad a los dos lados del cable—, que un cursor
/// ausente viaja como `null` explícito y no como un cero, y que un sondeo
/// vacío es `lines: []` con `lost: 0`. Las tres cosas son la diferencia entre
/// un panel que dice la verdad sobre lo que hubo y uno con un hueco callado.
fn check_methods_log(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        LOG_LEVELS, LogLevelParams, LogLevelResult, LogLine, LogTailParams, LogTailResult,
    };
    // Una fixtura por valor del vocabulario. El bucle va sobre `LOG_LEVELS`
    // para que añadir un nivel sin su fixtura se ponga rojo aquí, en vez de
    // pasar desapercibido hasta que un frontend no sepa colorearlo.
    for nivel in LOG_LEVELS {
        check_one(
            fixtures,
            &format!("log_level_params_{nivel}"),
            &LogLevelParams {
                level: (*nivel).to_owned(),
            },
        );
    }
    // El result no es un `bool`: el anillo NUNCA baja de nivel, así que pedir
    // `warn` con el anillo ya en `debug` contesta `debug`. Eso no es un fallo,
    // y con un `bool` habría que mentir con un `true` o alarmar con un `false`.
    check_one(
        fixtures,
        "log_level_result",
        &LogLevelResult {
            level: "debug".to_owned(),
        },
    );
    check_one(
        fixtures,
        "log_tail_params",
        &LogTailParams {
            cursor: Some(1234),
            max: 500,
        },
    );
    // `cursor` ausente serializa como `null` explícito (ADR 0004; `Option` sin
    // `skip`), y esa forma es la que manda un panel al abrirse. Es un caso
    // aparte a propósito: `null` quiere decir «lo que tengas» y `0` quiere
    // decir «desde la primera línea que existió», que contra un anillo que ya
    // dio la vuelta obligaría a contestar un `lost` enorme y falso.
    check_one(
        fixtures,
        "log_tail_params_sin_cursor",
        &LogTailParams {
            cursor: None,
            max: 500,
        },
    );
    // La segunda línea es de `suppaftp` y va en INFO: la cota del anillo deja
    // pasar lo de terceros hasta ahí y ni un nivel más, pase lo que pase con
    // `log.level`. Y el `target` viaja ENTERO, que es lo que hace posible esa
    // cota y también el filtro por subsistema del lector.
    check_one(
        fixtures,
        "log_tail_result",
        &LogTailResult {
            lines: vec![
                LogLine {
                    epoch_ms: 1_756_000_000_000,
                    level: "warn".to_owned(),
                    target: "norte_core::connect".to_owned(),
                    message: "la sesión de «trabajo» se degradó a texto en claro".to_owned(),
                },
                LogLine {
                    epoch_ms: 1_756_000_000_123,
                    level: "info".to_owned(),
                    target: "suppaftp".to_owned(),
                    message: "connected".to_owned(),
                },
            ],
            next: 4001,
            lost: 12,
            level: "info".to_owned(),
            capacity: 2000,
        },
    );
    // El sondeo que no encontró nada, que es la respuesta más frecuente: una
    // lista VACÍA y `lost: 0`. La fixtura existe porque `lines` no se omite —
    // el día que alguien le pusiera `skip_serializing_if`, «no ha pasado nada»
    // y «el campo no vino» dejarían de distinguirse en el cable.
    check_one(
        fixtures,
        "log_tail_result_vacio",
        &LogTailResult {
            lines: Vec::new(),
            next: 4001,
            lost: 0,
            level: "info".to_owned(),
            capacity: 2000,
        },
    );
}

/// `fs.dir_size` (0.49.0, #139): lo que se congela es que las rutas viajan
/// como una LISTA —una selección se mide de una vez— y con la forma de wire de
/// `VPath`, no como texto suelto.
fn check_methods_dir_size(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{ConnectionCloseParams, ConnectionCloseResult, FsDirSizeParams};
    // `connection.close` (#140): lo que se congela es que se cierra por una
    // RUTA —el frontend no tiene que saber cómo se llavea una sesión— y que el
    // result dice si había algo que cerrar.
    check_one(
        fixtures,
        "connection_close_params",
        &ConnectionCloseParams {
            path: norte_proto::VPath::parse("sftp://host/casa").expect("vpath"),
        },
    );
    check_one(
        fixtures,
        "connection_close_result",
        &ConnectionCloseResult { closed: true },
    );
    check_one(
        fixtures,
        "fs_dir_size_params",
        &FsDirSizeParams {
            paths: vec![
                norte_proto::VPath::parse("file:///a").expect("vpath"),
                norte_proto::VPath::parse("file:///b/c").expect("vpath"),
            ],
        },
    );
}

/// Familia de escritura de archivos (0.50.0, #132).
///
/// Lo que se congela: el FORMATO viaja como un token cerrado y explícito —no
/// se deduce del nombre en el servidor, ver `ARCHIVE_PACK`—, la BASE viaja
/// siempre porque sin ella los nombres guardados no están definidos, y el
/// resultado del test dice QUÉ comprobó además de qué falló: «pasa» significa
/// cosas distintas en un zip y en un tar plano, y sin `checked` un cliente
/// pintaría «íntegro» sobre un formato que no tiene con qué sostenerlo.
fn check_methods_archive_write(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        ArchiveFormat, ArchivePackParams, ArchiveTestFailure, ArchiveTestParams, ArchiveTestResult,
        FileCombineParams, FileSplitParams,
    };
    check_one(
        fixtures,
        "archive_pack_params",
        &ArchivePackParams {
            sources: vec![vpath("file:///proj/src"), vpath("file:///proj/LEEME")],
            dest: vpath("file:///proj.zip"),
            format: ArchiveFormat::TarGz,
            level: Some(9),
            base: vpath("file:///proj"),
        },
    );
    check_one(
        fixtures,
        "archive_test_params",
        &ArchiveTestParams {
            path: vpath("file:///a.zip"),
        },
    );
    check_one(
        fixtures,
        "archive_test_result",
        &ArchiveTestResult {
            entries: 3,
            failed: vec![ArchiveTestFailure {
                // La ruta ENTERA en forma wire: es la que señala CUÁL de las
                // dos `x.txt` de un archivo está corrupta, y la única que
                // conserva los bytes de un nombre que no es UTF-8.
                path: "zip+file:///a.zip/!/roto.txt".to_owned(),
                name: "roto.txt".to_owned(),
                reason: "crc".to_owned(),
            }],
            truncated: false,
            checked: vec!["crc".to_owned()],
        },
    );
    // Un archivo SANO, que es la respuesta corriente: sin fallos y diciendo
    // qué comprobó. Los tres tokens de `checked` son vocabulario del wire y
    // este golden es lo único que los congela.
    check_one(
        fixtures,
        "archive_test_result_clean",
        &ArchiveTestResult {
            entries: 9,
            failed: Vec::new(),
            truncated: false,
            checked: vec!["gzip_crc".to_owned()],
        },
    );
    check_one(
        fixtures,
        "archive_test_report_params",
        &norte_proto::methods::ArchiveTestReportParams {
            task_id: norte_proto::TaskId::new(7),
        },
    );
    check_methods_archive_pack_report(fixtures);
    check_methods_fs_checksum(fixtures);
    check_methods_fs_set_mode(fixtures);
    check_one(
        fixtures,
        "file_split_params",
        &FileSplitParams {
            path: vpath("file:///g.iso"),
            part_bytes: 1_048_576,
            dest_dir: vpath("file:///trozos"),
        },
    );
    check_one(
        fixtures,
        "file_combine_params",
        &FileCombineParams {
            first: vpath("file:///g.iso.001"),
            dest: vpath("file:///g.iso"),
        },
    );
}

/// `fs.set_mode` (0.60.0, #314): los permisos POSIX de un lote.
///
/// Con una ruta que NO es UTF-8, porque cambiarle los permisos a un fichero
/// cuyo nombre no es texto tiene que poder pedirse igual (regla 1), y con el
/// modo en su forma numérica: `0o755` viaja como 493, y congelarlo aquí es lo
/// que impide que alguien lo convierta a `"rwxr-xr-x"` sin darse cuenta de que
/// eso es un cambio de wire.
fn check_methods_fs_set_mode(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::FsSetModeParams;
    check_one(
        fixtures,
        "fs_set_mode_params",
        &FsSetModeParams {
            paths: vec![vpath("file:///casa/a.sh"), vpath("file:///casa/b%FF.bin")],
            mode: 0o755,
            recursive: false,
            dir_mode: None,
        },
    );
    // Y la forma RECURSIVA (0.62.0, #315), que es otra petición: los dos
    // campos presentes a la vez, porque `dir_mode` sin `recursive` no
    // significa nada. Una sola fixtura dejaría que el default de `recursive`
    // cambiara sin que nada se pusiera rojo.
    check_one(
        fixtures,
        "fs_set_mode_params_recursivo",
        &FsSetModeParams {
            paths: vec![vpath("file:///casa/arbol")],
            mode: 0o644,
            recursive: true,
            dir_mode: Some(0o755),
        },
    );
    // Y la TERCERA forma, que es la que rompe árboles: recursivo con el MISMO
    // modo para todo (`dir_mode` ausente). Es una petición distinta de las
    // otras dos y la que `chmod -R` hace, así que su wire se congela aparte.
    check_one(
        fixtures,
        "fs_set_mode_params_recursivo_un_modo",
        &FsSetModeParams {
            paths: vec![vpath("file:///casa/arbol")],
            mode: 0o600,
            recursive: true,
            dir_mode: None,
        },
    );
}

/// `fs.checksum` y su informe (0.59.0, #311): comprobar que un fichero es el
/// que alguien publicó.
///
/// La fixtura del informe lleva los DOS motivos de `miss` —lo único que congela
/// esos tokens del wire— y una ruta que NO es UTF-8: el informe tiene que poder
/// nombrar el fichero que no se pudo leer aunque su nombre no sea texto.
fn check_methods_fs_checksum(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        ChecksumAlgo, ChecksumEntry, ChecksumMiss, FsChecksumParams, FsChecksumReportParams,
        FsChecksumReportResult,
    };
    let vp = |w: &str| norte_proto::VPath::parse(w).expect("wire");
    check_one(
        fixtures,
        "fs_checksum_params",
        &FsChecksumParams {
            paths: vec![vp("file:///casa/a.txt"), vp("file:///casa/b%FF.bin")],
            algo: ChecksumAlgo::Sha256,
        },
    );
    check_one(
        fixtures,
        "fs_checksum_report_params",
        &FsChecksumReportParams {
            task_id: norte_proto::TaskId::new(9),
        },
    );
    check_one(
        fixtures,
        "fs_checksum_report_result",
        &FsChecksumReportResult {
            entries: vec![
                ChecksumEntry {
                    path: vp("file:///casa/a.txt"),
                    digest: Some(
                        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                            .to_owned(),
                    ),
                    miss: None,
                },
                ChecksumEntry {
                    path: vp("file:///casa/b%FF.bin"),
                    digest: None,
                    miss: Some(ChecksumMiss::Unreadable),
                },
                ChecksumEntry {
                    path: vp("file:///casa/sub"),
                    digest: None,
                    miss: Some(ChecksumMiss::NotAFile),
                },
            ],
            algo: ChecksumAlgo::Sha256,
            // A medias A PROPÓSITO: `pending > 0` es lo que un informe de una
            // Task cancelada deja escrito, y congelarlo aquí es lo que impide
            // que alguien lo ponga a cero «por limpieza».
            pending: 2,
        },
    );
}

/// `archive.pack_report` (0.58.0, #250): lo que ese empaquetado guardó y que
/// significa otra cosa fuera.
///
/// Los cuatro tokens de `risk` son vocabulario del wire y estos goldens son lo
/// único que los congela — uno por valor, porque una sola fixture dejaría
/// renombrar los otros tres sin que nada se enterara.
///
/// Lo que NO tiene fixture son las colisiones por plegado, y es deliberado:
/// esas no se empaquetan —`archive.pack` falla con `Exists` antes de escribir
/// un byte— así que el informe no tiene dónde llevarlas.
fn check_methods_archive_pack_report(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{ArchivePackReportParams, ArchivePackReportResult, PackRiskyName};
    check_one(
        fixtures,
        "archive_pack_report_params",
        &ArchivePackReportParams {
            task_id: norte_proto::TaskId::new(7),
        },
    );
    check_one(
        fixtures,
        "archive_pack_report_result",
        &ArchivePackReportResult {
            entries: 5,
            checked: vec![
                "separator".to_owned(),
                "stream".to_owned(),
                "reserved".to_owned(),
                "trailing".to_owned(),
            ],
            risky: vec![
                PackRiskyName {
                    path: "a%5Cb.txt".to_owned(),
                    name: "a\\b.txt".to_owned(),
                    risk: "separator".to_owned(),
                },
                // Un nombre que NO es UTF-8: `path` es lo único de lo que se
                // recuperan los bytes —`name` trae el `U+FFFD` de pintarlo—, y
                // sin esta fixture la propiedad por la que existe ese codec no
                // la congelaba nada (regla 1).
                PackRiskyName {
                    path: "malo%FF%5Cx.txt".to_owned(),
                    name: "malo\u{fffd}\\x.txt".to_owned(),
                    risk: "separator".to_owned(),
                },
                PackRiskyName {
                    path: "f%3Aads".to_owned(),
                    name: "f:ads".to_owned(),
                    risk: "stream".to_owned(),
                },
                PackRiskyName {
                    path: "CON".to_owned(),
                    name: "CON".to_owned(),
                    risk: "reserved".to_owned(),
                },
                PackRiskyName {
                    path: "nombre.".to_owned(),
                    name: "nombre.".to_owned(),
                    risk: "trailing".to_owned(),
                },
            ],
            truncated: false,
        },
    );
    // Y el informe LIMPIO, que es la respuesta corriente y la que dice algo por
    // sí sola: se comprobaron doce entradas y no había nada.
    check_one(
        fixtures,
        "archive_pack_report_result_clean",
        &ArchivePackReportResult {
            entries: 12,
            checked: vec![
                "separator".to_owned(),
                "stream".to_owned(),
                "reserved".to_owned(),
                "trailing".to_owned(),
            ],
            ..Default::default()
        },
    );
}

/// Familia `session.*` de UI (0.48.0, L2): la pantalla que el daemon guarda.
/// El golden congela que `body` viaja TAL CUAL —un objeto arbitrario, ni
/// envuelto ni re-serializado a string— y que `owner` va en el result del GET
/// y no dentro de la sesión: quién manda es del CANAL, no del documento.
fn check_methods_ui_session(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{Session, SessionGetResult, SessionPutParams, SessionPutResult};
    let body = serde_json::json!({ "slots": { "1": { "cursor": 12 } } });
    check_one(
        fixtures,
        "session_get_result",
        &SessionGetResult {
            session: Session {
                version: 1,
                revision: 3,
                body: body.clone(),
            },
            owner: true,
        },
    );
    // La sesión VACÍA, que es la respuesta más común de todo este wire: la de
    // cada primer `session.get` de cada instalación. `body` es `null` y NO
    // `{}` — el único caso en que no es un objeto—, así que si algo lo
    // cambiara a `{}` ningún otro golden se enteraría.
    check_one(
        fixtures,
        "session_get_result_empty",
        &SessionGetResult {
            session: Session::default(),
            owner: false,
        },
    );
    check_one(
        fixtures,
        "session_put_params",
        &SessionPutParams {
            version: 1,
            revision: 3,
            body,
        },
    );
    check_one(
        fixtures,
        "session_put_result",
        &SessionPutResult { revision: 4 },
    );
}

/// Familia `fs.rename_batch*` (0.36.0): las PETICIONES de plan y de ejecución.
/// La intención (`pairs`) es lo único que el cliente manda — el orden lo decide
/// el core —, y la ejecución añade el `plan_hash` que el humano aprobó.
fn check_methods_rename_batch(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        FsRenameBatchParams, FsRenameBatchPlanParams, FsRenameBatchReportParams,
        FsRenameBatchReportResult, RenamePair, RenameStuckStep,
    };
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
    check_one(
        fixtures,
        "fs_rename_batch_report_params",
        &FsRenameBatchReportParams {
            task_id: norte_proto::TaskId::new(7),
        },
    );
    // El informe que MOTIVA el método: el rollback se atascó, así que hay un
    // fichero bajo un nombre que nadie pidió y el informe lo NOMBRA. Con el
    // nombre hostil, que es donde un `String` habría mentido.
    check_one(
        fixtures,
        "fs_rename_batch_report_result",
        &FsRenameBatchReportResult {
            applied: 2,
            rolled_back: 1,
            failed_pair: Some(1),
            stuck: Some(RenameStuckStep {
                from: vpath("file:///home/user/fotos/caf%FF.txt"),
                to: vpath("file:///home/user/fotos/.norte-rename-0a1b2c3d-0"),
                pair_index: 0,
                error: norte_proto::Error::Io { retryable: false },
                journalled: true,
                still_applied: 1,
            }),
            uncertain: None,
            compensations_lost: 0,
        },
    );
    // La forma que NINGUNA otra fixture cubre: el paso cuyo destino se
    // desconoce (`uncertain`), sin entrada de journal detrás (`journalled:
    // false` — nadie lo va a deshacer, solo un humano) y con compensaciones
    // perdidas. Es el peor desenlace posible y es exactamente por el que existe
    // el método: si su forma no está congelada, no lo está la que importa.
    check_one(
        fixtures,
        "fs_rename_batch_report_result_uncertain",
        &FsRenameBatchReportResult {
            applied: 1,
            rolled_back: 1,
            failed_pair: Some(0),
            stuck: None,
            uncertain: Some(RenameStuckStep {
                from: vpath("file:///home/user/fotos/a"),
                to: vpath("file:///home/user/fotos/b"),
                pair_index: 0,
                error: norte_proto::Error::ProviderUnavailable { retryable: true },
                journalled: false,
                still_applied: 1,
            }),
            compensations_lost: 2,
        },
    );
    // Corrida limpia: lo ausente se OMITE, y `compensations_lost` viaja en
    // cero como el resto de contadores.
    check_one(
        fixtures,
        "fs_rename_batch_report_result_clean",
        &FsRenameBatchReportResult {
            applied: 3,
            rolled_back: 0,
            failed_pair: None,
            stuck: None,
            uncertain: None,
            compensations_lost: 0,
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
            names: Vec::new(),
        },
    );
    // El plan sobre la SELECCIÓN (0.62.0, #121): los nombres viajan y el
    // directorio sigue siendo el mismo. Fixtura aparte porque el campo se
    // OMITE cuando está vacío — con una sola, el día que deje de omitirse
    // nadie se entera.
    check_one(
        fixtures,
        "ai_rename_plan_params_seleccion",
        &AiRenamePlanParams {
            dir: VPath::parse("file:///home/user/fotos").unwrap(),
            instruction: "kebab-case".into(),
            names: vec!["IMG 001.jpg".into(), "IMG 002.jpg".into()],
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
            refused: None,
        },
    );
    // Plan vacío = el modelo no propuso cambios (estado significativo, no un
    // caso omitido): fija la forma del wire, no solo el caso feliz.
    check_one(
        fixtures,
        "ai_rename_plan_result_empty",
        &AiRenamePlanResult {
            entries: vec![],
            refused: None,
        },
    );
    // 0.68.0 (#332): un plan vacío CON motivo — el plugin rehusó y dijo por
    // qué. Fixture aparte: las dos de arriba prueban que el wire de 0.67 no
    // se movió; esta, que el campo existe.
    check_one(
        fixtures,
        "ai_rename_plan_result_refused",
        &AiRenamePlanResult {
            entries: vec![],
            refused: Some("this renamer needs the `location` capability".into()),
        },
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
            manifest_digest: None,
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
// Una lista LITERAL de casos golden: cada uno es una forma congelada del wire
// con su porqué, y partirla en mitades arbitrarias solo escondería cuáles hay.
#[expect(
    clippy::too_many_lines,
    reason = "cada fixture lleva su porqué; partir en mitades escondería cuáles hay"
)]
/// `plugin.notice` (0.69.0, ADR 0100): UNA fixtura POR VALOR de `kind`. Con
/// una sola, renombrar la otra no pondría nada en rojo, y el frontend compara
/// `kind` por igualdad para decidir si traduce la clase o pinta `text`.
fn check_methods_plugin_notice(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{PLUGIN_NOTICE_KINDS, PluginNotice};
    assert_eq!(PLUGIN_NOTICE_KINDS, &["notify", "hooks-disabled"]);
    check_one(
        fixtures,
        "plugin_notice_notify",
        &PluginNotice {
            plugin_id: "org.norte.rename-log".into(),
            kind: "notify".into(),
            text: Some("renamed 3 files".into()),
        },
    );
    // Sin `text`: la prueba de que no viaja cuando no lo hay.
    check_one(
        fixtures,
        "plugin_notice_hooks_disabled",
        &PluginNotice {
            plugin_id: "org.norte.rename-log".into(),
            kind: "hooks-disabled".into(),
            text: None,
        },
    );
}

fn check_methods_plugin_info(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        PluginColumnInfo, PluginCommandInfo, PluginCommandKind, PluginInfo, PluginListResult,
        PluginLoadError,
    };
    check_one(
        fixtures,
        "plugin_command_info",
        &PluginCommandInfo {
            id: "greet".into(),
            title: "Greet".into(),
            kind: PluginCommandKind::Command,
        },
    );
    // 0.67.0 (ADR 0095): un renamer entre los comandos, con su `kind`. La
    // fixtura de arriba prueba que un comando sigue sin llevarlo.
    check_one(
        fixtures,
        "plugin_command_info_renamer",
        &PluginCommandInfo {
            id: "by-date".into(),
            title: "Rename by date".into(),
            kind: PluginCommandKind::Renamer,
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
            manifest_digest: None,
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
                    kind: PluginCommandKind::Command,
                },
                PluginCommandInfo {
                    id: "wave".into(),
                    title: "Wave".into(),
                    kind: PluginCommandKind::Command,
                },
            ],
            columns: vec![PluginColumnInfo {
                id: "git-status".into(),
                header: "Git".into(),
            }],
            has_help: false,
            manifest_digest: None,
        },
    );
    // 0.53.0 (#282): el ancla que viaja con el catálogo y vuelve con el sí.
    check_one(
        fixtures,
        "plugin_info_with_digest",
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
            manifest_digest: Some(
                "7aec1a5a3d48445efc60e4ede6a6257fc1b2a651f2c89e625228982440307376".into(),
            ),
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
                manifest_digest: None,
            }],
            errors: vec![PluginLoadError {
                // El BASENAME, nunca la ruta absoluta: ésta revelaría el home
                // del usuario a un agente que llame a `plugin.list`, y el
                // rustdoc del campo lo declara invariante. El golden anterior
                // congelaba `/plugins/broken`, o sea el contrario.
                dir: "broken".into(),
                reason: "manifiesto inválido".into(),
                dir_bytes: None,
            }],
        },
    );
    // 0.53.0 (#265): con los bytes del basename al lado. Es el caso que
    // CONGELA la forma base64 de `label_wire` para este campo — sin él, el
    // alfabeto y el relleno no los fija nada, y la ausencia del otro caso no
    // fija ni siquiera el NOMBRE `dir_bytes`.
    check_one(
        fixtures,
        "plugin_list_result_dir_bytes",
        &PluginListResult {
            plugins: vec![],
            errors: vec![PluginLoadError {
                dir: "caf\u{FFFD}".into(),
                reason: "manifiesto inválido".into(),
                // `caf\xff`: los bytes que la cadena de arriba ya no puede
                // decir, que es la razón de ser del campo.
                dir_bytes: Some(vec![b'c', b'a', b'f', 0xFF]),
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
    check_methods_plugin_notice(fixtures);
    check_one(
        fixtures,
        "plugin_set_approval_params",
        &PluginSetApprovalParams {
            id: "org.norte.demo".into(),
            approved: true,
            expected_digest: None,
        },
    );
    // 0.53.0 (#282): con el ancla que el humano leyó. Congela el nombre del
    // campo y su forma (hex minúscula de un sha256), que es lo que el daemon
    // compara byte a byte antes de conceder.
    check_one(
        fixtures,
        "plugin_set_approval_params_anchored",
        &PluginSetApprovalParams {
            id: "org.norte.demo".into(),
            approved: true,
            expected_digest: Some(
                "7aec1a5a3d48445efc60e4ede6a6257fc1b2a651f2c89e625228982440307376".into(),
            ),
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

/// 0.66.0 (D4): el fondo de un span, y el ancho del visor en la petición.
/// Fixturas APARTE de las de 0.27.0: los dos campos se omiten cuando faltan,
/// así que aquellas prueban que el wire viejo no se movió y estas que el
/// nuevo existe.
fn check_methods_plugin_preview_styled_066(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{PluginPreviewStyledParams, SpanWire};
    check_one(
        fixtures,
        "span_wire_bg",
        &SpanWire {
            text: "▀".into(),
            role: None,
            fg: Some([255, 0, 0]),
            bg: Some([0, 0, 255]),
        },
    );
    check_one(
        fixtures,
        "plugin_preview_styled_params_columns",
        &PluginPreviewStyledParams {
            path: vpath("file:///home/user/photo.png"),
            columns: Some(80),
        },
    );
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
            bg: None,
        },
    );
    check_one(
        fixtures,
        "span_wire_styled",
        &SpanWire {
            text: "año".into(),
            role: Some("match".into()),
            fg: Some([200, 40, 40]),
            bg: None,
        },
    );
    check_methods_plugin_preview_styled_066(fixtures);
    check_one(
        fixtures,
        "plugin_preview_styled_params",
        &PluginPreviewStyledParams {
            path: vpath("file:///home/user/doc.rs"),
            columns: None,
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
                            bg: None,
                        },
                        SpanWire {
                            text: " main".into(),
                            role: None,
                            fg: None,
                            bg: None,
                        },
                    ],
                    vec![SpanWire {
                        text: "año".into(),
                        role: None,
                        fg: Some([255, 0, 0]),
                        bg: None,
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
                    bg: None,
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
    // 0.67.0 (ADR 0095): el plan de un renamer. El result es el de la IA y
    // ya tiene su fixtura.
    check_one(
        fixtures,
        "plugin_rename_plan_params",
        &norte_proto::methods::PluginRenamePlanParams {
            plugin_id: "org.norte.date-prefix".into(),
            renamer_id: "by-date".into(),
            dir: vpath("file:///home/user/fotos"),
            names: vec!["a.jpg".into(), "b.jpg".into()],
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
        PolicyUndoSessionResult, RenameStuckStep, UndoBlocked,
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
    // 0.36.0 (batch rename): `batch_stuck` viaja JUNTO a `blocked` y no en su
    // lugar — dicen cosas distintas («paré, el árbol está consistente» frente a
    // «no pude devolverlo»), y una fixture que solo pudiera llevar uno de los
    // dos dejaría creer que se excluyen.
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
            batch_stuck: Some(RenameStuckStep {
                from: vpath("file:///home/user/fotos/a%FF"),
                to: vpath("file:///home/user/fotos/b"),
                pair_index: 0,
                error: norte_proto::Error::Io { retryable: false },
                journalled: true,
                still_applied: 2,
            }),
            compensations_lost: 1,
            // 0.43.0 (#171): la policy denegó una unidad y el undo SIGUIÓ. Va
            // en la misma fixture que `blocked` a propósito: los dos pueden
            // salir juntos y dicen cosas opuestas —«paré» frente a «me salté
            // ésta y continué»—, así que una fixture que solo pudiera llevar
            // uno dejaría creer que se excluyen.
            denied: vec![UndoBlocked {
                seq: 37,
                error: norte_proto::Error::PolicyDenied {
                    rule: "scope-expired".into(),
                },
            }],
            denied_total: 1,
        },
    );
    // Sin bloqueo: `blocked` y `batch_stuck` se OMITEN (skip_serializing_if),
    // no `null`. `compensations_lost` sí viaja en cero, como los otros
    // contadores: un contador ausente y un contador en cero no deben poder
    // confundirse.
    check_one(
        fixtures,
        "policy_undo_report_result_clean",
        &PolicyUndoReportResult {
            undone: 4,
            skipped_irreversible: 0,
            skipped_created_no_trash: 0,
            blocked: None,
            batch_stuck: None,
            compensations_lost: 0,
            // Vacía y en cero: como `compensations_lost`, viajan igual — un
            // contador ausente y uno en cero no deben poder confundirse.
            denied: Vec::new(),
            denied_total: 0,
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
            // 0.36.0: la lista está RECORTADA — una ruta enseñada de nueve. Es
            // la forma que importa congelar: con `paths_total == paths.len()`
            // la fixture no demostraría nada, y es justo el caso en el que un
            // frontend tiene que avisar al humano.
            paths_total: 9,
            ttl_ms: 30_000,
            // 0.61.0 (#314): la op que NO se contesta con la op y las rutas.
            // Se congela la forma CON modo: es lo que hace falta que viaje, y
            // la de sin él la cubren las dos fixturas de `pending_approval`,
            // donde el campo se omite entero.
            detail: norte_proto::methods::ApprovalDetail {
                mode: Some(0o755),
                recursive: false,
                dir_mode: None,
            },
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
            paths_total: 9,
            // Sin detalle: un `delete` se contesta con la op y las rutas, y el
            // campo se OMITE del JSON entero — que es lo que hace que el bump
            // sea aditivo para todas las demás ops.
            detail: norte_proto::methods::ApprovalDetail::default(),
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
                paths_total: 9,
                detail: norte_proto::methods::ApprovalDetail::default(),
            }],
        },
    );
}

/// Familia connection.* (0.7.0, fase 6): `trust_host_key` del flujo TOFU, y
/// `provide_secret` (0.63.0, #325), que es su gemelo.
fn check_methods_connection(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        ConnectionDegraded, ConnectionFailed, ConnectionProvideSecretParams,
        ConnectionProvideSecretResult, ConnectionTrustHostKeyParams, ConnectionTrustHostKeyResult,
    };
    // #325. El `conn` lleva acentos y eñe a propósito: es una CLAVE de
    // `connections.toml`, o sea UTF-8 cualquiera, y este fixture es lo que
    // impide que alguien la normalice o la recorte de camino al cable. El
    // `secret` es inventado: un fixture no es un secreto, y sin él nada
    // congela la forma de los params (que es el argumento del propio
    // fichero).
    check_one(
        fixtures,
        "connection_provide_secret_params",
        &ConnectionProvideSecretParams {
            conn: "coágulo-ñandú".to_owned(),
            secret: "hunter2".to_owned(),
        },
    );
    check_one(
        fixtures,
        "connection_provide_secret_result",
        &ConnectionProvideSecretResult { stored: true },
    );
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

    // `connection.failed` (0.64.0, #322): UN fixture POR VALOR del vocabulario
    // cerrado. Con uno solo, renombrar cualquiera de los otros seis no pondría
    // nada en rojo — y `reason` se compara por igualdad en el frontend, así que
    // un renombrado silencioso es una frase que deja de salir.
    for (caso, reason) in [
        ("connection_failed_secret_missing", "secret-missing"),
        ("connection_failed_secret_empty", "secret-empty"),
        ("connection_failed_secret_not_utf8", "secret-not-utf8"),
        ("connection_failed_secret_store", "secret-store"),
        ("connection_failed_auth_rejected", "auth-rejected"),
        ("connection_failed_no_user", "no-user"),
        ("connection_failed_agent", "agent"),
    ] {
        check_one(
            fixtures,
            caso,
            &ConnectionFailed {
                conn: Some("trabajo".into()),
                scheme: "sftp".into(),
                host: "servidor.example".into(),
                reason: reason.into(),
                detail: Some("el secreto de «trabajo» está definido pero VACÍO".into()),
            },
        );
    }
    // Y el caso SIN los dos opcionales: la prueba de que no viajan cuando no
    // están (una URL tecleada no tiene nombre de conexión).
    check_one(
        fixtures,
        "connection_failed_sin_opcionales",
        &ConnectionFailed {
            conn: None,
            scheme: "sftp".into(),
            host: "servidor.example".into(),
            reason: "auth-rejected".into(),
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
#[expect(
    clippy::too_many_lines,
    reason = "una fixtura por método de la familia fs.*, sin lógica dentro"
)]
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
            dir_anchor: None,
        },
    );
    check_one(
        fixtures,
        "fs_list_result_paginado",
        &FsListResult {
            entries: vec![sample_entry.clone()],
            next_cursor: Some("3".to_owned()),
            skipped: None,
            dir_anchor: None,
        },
    );
    check_one(
        fixtures,
        "fs_list_result_con_skipped",
        &FsListResult {
            entries: vec![sample_entry.clone()],
            next_cursor: None,
            skipped: Some(3),
            dir_anchor: None,
        },
    );
    // 0.54.0 (#295): la identidad OPACA del directorio listado, que el cliente
    // retiene para poder decir DESPUÉS cuál era. Congela el nombre del campo y
    // su forma en el wire —una cadena hex, nunca un inodo ni un volumen—, que
    // es lo único que un cliente puede ver de ella.
    check_one(
        fixtures,
        "fs_list_result_anchored",
        &FsListResult {
            entries: vec![sample_entry.clone()],
            next_cursor: None,
            skipped: None,
            dir_anchor: Some(norte_proto::DirAnchor::new(
                "3f2a91c40b7d6e58aa10c4d9f8e37b62".to_owned(),
            )),
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
    // 0.57.0 (#290): fs.create. Con un nombre PERCENT-ENCODED, que es el
    // motivo por el que estas fixturas existen: lo que hay que congelar no es
    // que el campo se llame `path`, es que un nombre con bytes que no son
    // ASCII imprimible cruza el cable y vuelve IGUAL (regla dura 1).
    check_one(
        fixtures,
        "fs_create_params",
        &norte_proto::methods::FsCreateParams {
            path: vpath("file:///tmp/borrador-%FF%FE.txt"),
            dest_anchor: None,
        },
    );
    // Y con ancla: `fs.create` la lleva y `fs.mkdir` no, así que la pareja
    // omitida/presente hace falta aquí igual que en copiar y mover.
    check_one(
        fixtures,
        "fs_create_params_anchored",
        &norte_proto::methods::FsCreateParams {
            path: vpath("file:///tmp/borrador-%FF%FE.txt"),
            dest_anchor: Some(norte_proto::DirAnchor::new(
                "3f2a91c40b7d6e58aa10c4d9f8e37b62".to_owned(),
            )),
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

/// `fs.compare` + `compare.rows` (0.39.0, ADR 0048): la PETICIÓN y el LOTE.
///
/// `fs_compare_params_minimo` es el que importa: dos raíces y nada más, y aun
/// así el wire lleva `criteria` ENTERO, `mtime_tolerance_ms` y
/// `follow_symlinks`. Esos defaults deciden si comparar dos árboles lee
/// contenido (`hash: false`) y qué cuenta como «la misma fecha» (2000 ms, la
/// regla FAT), así que se congelan explícitos en vez de omitirse: un peer que
/// los dedujera al revés leería un terabyte que nadie pidió.
fn check_methods_compare(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        CompareConfidence, CompareCriteria, CompareCriterion, CompareRowsBatch, CompareVerdict,
        DescendSide, FsCompareParams,
    };
    check_one(
        fixtures,
        "fs_compare_params_minimo",
        &FsCompareParams {
            left: vpath("file:///home/user/origen"),
            right: vpath("file:///home/user/copia"),
            criteria: CompareCriteria::default(),
            max_depth: None,
            mtime_tolerance_ms: 2000,
            follow_symlinks: false,
            // Ausente en la fixture, y esa ausencia ES el comportamiento de
            // 0.39.0: un huérfano, una fila. La petición mínima de un cliente
            // 0.39 no cambia ni un byte con el campo nuevo (0.40.0).
            descend_orphans: None,
        },
    );
    // Todo poblado, y con raíces HOSTILES: `max_depth` presente (se omite
    // cuando es `None`, y esta fixture es la que lo demuestra por contraste),
    // el rung caro encendido y tolerancia CERO — un filesystem que promete
    // nanosegundos a los dos lados.
    check_one(
        fixtures,
        "fs_compare_params",
        &FsCompareParams {
            left: vpath("file:///home/user/fotos-a%FF%FE"),
            right: vpath("sftp://nas/fotos-a%FF%FE"),
            criteria: CompareCriteria {
                size: true,
                mtime: true,
                hash: true,
            },
            max_depth: Some(3),
            mtime_tolerance_ms: 0,
            follow_symlinks: false,
            descend_orphans: Some(DescendSide::Left),
        },
    );
    // El lote: `task_id` para correlacionar y las filas en el orden en que el
    // walk las produjo. Nunca más de `COMPARE_ROWS_MAX_BATCH`.
    check_one(
        fixtures,
        "compare_rows_batch",
        &CompareRowsBatch {
            task_id: TaskId::new(7),
            rows: vec![compare_row(
                1,
                Some(compare_entry(
                    "file:///home/user/origen/informe%FF%FE.dat",
                    EntryKind::File,
                    Some(1234),
                    Some(1_720_000_000_000),
                )),
                None,
                CompareVerdict::OnlyLeft,
                CompareCriterion::Presence,
                CompareConfidence::Certain,
            )],
        },
    );
    // Un lote VACÍO es una lista vacía en el wire, jamás una clave ausente: el
    // pump del core puede cerrar la comparación sin filas nuevas.
    check_one(
        fixtures,
        "compare_rows_batch_empty",
        &CompareRowsBatch {
            task_id: TaskId::new(7),
            rows: vec![],
        },
    );
}

/// Familia `sync.*` (0.40.0, ADR 0049): la PETICIÓN del plan, sus dos
/// notificaciones, la aplicación —que no lleva más que el hash— y el informe.
///
/// Lo que congela, más allá de los nombres de campo:
///
/// - `sync_plan_params_minimo` es lo mínimo que un cliente manda —dos raíces y
///   el modo— con TODO lo demás en su default, que es lo que hace de esta
///   fixture el ancla de esos defaults. El modo no tiene default y por eso no
///   puede faltar: entre copiar y borrar no hay valor neutro.
/// - `sync_plan_params` lleva las dos raíces HOSTILES y CRUZANDO PROVIDER
///   (local → sftp), `include` poblado, el rung caro encendido y
///   `on_unknown: skip`. Ni `descend_orphans` ni `follow_symlinks` aparecen: no
///   son del llamante, y mandarlos es `-32602`.
/// - `sync_plan_done_blocked` congela la forma —no el número— de la lista
///   recortada: `blockers` es lo que cabe y `blockers_total` lo que hubo.
///   Y `executable: false` viaja aunque se pudiera deducir de la lista, por el
///   mismo motivo que en `FsRenameBatchPlanResult`.
/// - `sync_apply_params` tiene UNA clave. Es el invariante entero del método.
/// - `sync_report_result_died` es la única forma en la que `batch_id` falta: la
///   aplicación murió antes de abrir la unidad del journal. Un informe sin
///   `batch_id` es un informe sin undo, así que la clave ausente es una
///   afirmación fuerte y tiene fixture propia.
fn check_methods_sync(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        CompareCriteria, OnUnknown, SyncCompareOptions, SyncMode, SyncPlanParams,
    };
    check_one(
        fixtures,
        "sync_plan_params_minimo",
        &SyncPlanParams {
            source: vpath("file:///home/user/origen"),
            dest: vpath("file:///home/user/copia"),
            mode: SyncMode::Update,
            compare: SyncCompareOptions::default(),
            on_unknown: OnUnknown::Copy,
            include: None,
        },
    );
    check_one(
        fixtures,
        "sync_plan_params",
        &SyncPlanParams {
            source: vpath("file:///home/user/fotos-a%FF%FE"),
            dest: vpath("sftp://nas/fotos-a%FF%FE"),
            mode: SyncMode::Mirror,
            compare: SyncCompareOptions {
                criteria: CompareCriteria {
                    size: true,
                    mtime: true,
                    hash: true,
                },
                max_depth: Some(3),
                mtime_tolerance_ms: 0,
                follow_symlinks: false,
                descend_orphans: None,
            },
            on_unknown: OnUnknown::Skip,
            include: Some(vec![rel_path("informe%FF%FE.dat"), rel_path("sub/fotos")]),
        },
    );
}

/// Las dos NOTIFICACIONES del plan: los lotes de pasos y el cierre.
fn check_methods_sync_notifs(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, DestTrash, Side, StepReversal, SyncBlocker,
        SyncBlockerKind, SyncCounts, SyncPlanDone, SyncStep, SyncStepKind, SyncStepsBatch,
    };
    check_one(
        fixtures,
        "sync_steps_batch",
        &SyncStepsBatch {
            task_id: TaskId::new(7),
            steps: vec![SyncStep {
                id: 1,
                kind: SyncStepKind::Copy,
                rel: rel_path("informe%FF%FE.dat"),
                dest_rel: None,
                size: Some(1234),
                criterion: CompareCriterion::Presence,
                confidence: CompareConfidence::Certain,
                reversal: Some(StepReversal::Delete),
                reason: None,
            }],
        },
    );
    // Un lote VACÍO es una lista vacía, jamás una clave ausente: el pump puede
    // cerrar un plan sin pasos nuevos que mandar.
    check_one(
        fixtures,
        "sync_steps_batch_empty",
        &SyncStepsBatch {
            task_id: TaskId::new(7),
            steps: vec![],
        },
    );
    check_one(
        fixtures,
        "sync_plan_done",
        &SyncPlanDone {
            task_id: TaskId::new(7),
            plan_hash: plan_hash(&"1".repeat(64)),
            counts: SyncCounts {
                create_dir: 2,
                copy: 40,
                overwrite: 3,
                delete_tree: 1,
                skip: 2,
                // No nulo A PROPÓSITO: un cliente N-1 sumando los lotes de un
                // daemon N+1 es el único que lo llena, y el golden tiene que
                // enseñar que la clave viaja.
                unknown_kind: 2,
                // Igual que `unknown_kind`: un `irreversible` junto a una
                // papelera restaurable NO lo produce este core —`reversal_for`
                // no marca irreversible lo que la papelera puede devolver—, así
                // que esta fixture es la forma de un daemon N+1, congelada a
                // propósito para que un cliente sepa leerla.
                irreversible: 1,
                bytes: 4096,
                unmeasured_steps: 7,
            },
            blockers: vec![],
            blockers_total: 0,
            executable: true,
            // La papelera que SÍ devuelve las cosas: es lo que hace verdad el
            // `delete`/`restore_trash` de los pasos de este mismo plan.
            dest_trash: DestTrash::Restorable,
        },
    );
    check_one(
        fixtures,
        "sync_plan_done_blocked",
        &SyncPlanDone {
            task_id: TaskId::new(8),
            plan_hash: plan_hash(&"2".repeat(64)),
            counts: SyncCounts {
                copy: 1,
                bytes: 10,
                ..SyncCounts::default()
            },
            blockers: vec![SyncBlocker {
                rel: rel_path("LEEME%FF.txt"),
                kind: SyncBlockerKind::AmbiguousDest,
                side: Some(Side::Right),
            }],
            blockers_total: 300,
            executable: false,
            // Y la que no existe: el `copy` de arriba dice `delete` y aun así
            // no volvería (el undo lo salta). El golden congela la pareja
            // porque es la que un diálogo no puede distinguir sin este campo.
            dest_trash: DestTrash::Absent,
        },
    );
    // La tercera papelera: la de macOS y Windows, que entierra sin decir dónde.
    // Ahí NINGÚN paso es reversible —ni una copia— y por eso `irreversible`
    // iguala a la suma de las clases que actúan.
    check_one(
        fixtures,
        "sync_plan_done_opaque",
        &SyncPlanDone {
            task_id: TaskId::new(9),
            plan_hash: plan_hash(&"3".repeat(64)),
            counts: SyncCounts {
                copy: 1,
                overwrite: 1,
                irreversible: 2,
                bytes: 20,
                ..SyncCounts::default()
            },
            blockers: vec![],
            blockers_total: 0,
            executable: true,
            dest_trash: DestTrash::Opaque,
        },
    );
}

/// La segunda mitad de la familia: aplicar un plan aprobado, y su informe.
fn check_methods_sync_apply(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        DestTrash, SyncApplyParams, SyncFailure, SyncFailureCause, SyncReportParams,
        SyncReportResult, SyncStepKind,
    };
    check_one(
        fixtures,
        "sync_apply_params",
        &SyncApplyParams {
            plan_hash: plan_hash(&"1".repeat(64)),
        },
    );
    check_one(
        fixtures,
        "sync_report_params",
        &SyncReportParams {
            task_id: TaskId::new(9),
        },
    );
    check_one(
        fixtures,
        "sync_report_result",
        // `failed` cuenta TODOS los fallos y `failures` es la lista recortada,
        // así que `failures.len() <= failed` siempre. Con cinco filas y un
        // `failed: 4` la golden enseñaría lo contrario a quien la lea para
        // escribir un cliente.
        &SyncReportResult {
            done: 40,
            failed: 5,
            skipped: 2,
            bytes: 4096,
            failures: vec![
                SyncFailure {
                    rel: rel_path("a.txt"),
                    dest_rel: None,
                    cause: SyncFailureCause::Conflict,
                    kind: SyncStepKind::Overwrite,
                },
                // La fila hostil MÁS corriente de un `Mirror`, y la que motivó
                // `SyncFailure::kind` (0.42.0, #195): un `DeleteTree` denegado.
                // No lleva `dest_rel` —no hay pareja que deletrear— y su `rel`
                // cuelga del DESTINO, así que antes de este campo la única
                // prueba en el wire (`dest_rel` presente ⟹ `rel` es del origen)
                // no decía nada y el lector tenía que elegir una raíz a ciegas.
                SyncFailure {
                    rel: rel_path("b%FF.txt"),
                    dest_rel: None,
                    cause: SyncFailureCause::Denied,
                    kind: SyncStepKind::DeleteTree,
                },
                SyncFailure {
                    rel: rel_path("sub/c.txt"),
                    dest_rel: None,
                    cause: SyncFailureCause::Io,
                    kind: SyncStepKind::Copy,
                },
                // La cuarta clase que el ejecutor puede anotar, y la que el
                // barrido del esquema NO cazaría: `SyncStepKind` se pinea
                // contra `sync_step.json`, así que una clase sin fila de FALLO
                // aquí pasaría desapercibida. `Skip` no puede: no se ejecuta,
                // así que no falla (`protocol-guardian`, W4b MINOR-4).
                SyncFailure {
                    rel: rel_path("sub"),
                    dest_rel: None,
                    cause: SyncFailureCause::Denied,
                    kind: SyncStepKind::CreateDir,
                },
                // La legalidad del nombre bajo la raíz de DESTINO no se valida al
                // planificar, así que aflora aquí y con nombre propio. Y con la
                // grafía del DESTINO, que es la que falló: el caso estrella es
                // un nombre que revienta `NAME_MAX` al recomponerse en NFD, y
                // enseñar `rel` a secas señalaría la grafía corta y legal del
                // origen.
                //
                // La pareja que se CONGELA aquí es la plegada por caja y no la
                // NFC/NFD, por lo que ya avisó la golden de `dest_rel` en
                // `SyncStep`: las dos formas Unicode son UTF-8 válido y el códec
                // de segmento las deja literales, así que en un fichero JSON
                // renderizan IGUAL — el diff sería inadjudicable y una
                // normalización del editor convertiría el test en una
                // tautología.
                SyncFailure {
                    rel: rel_path("NOTAS/informe.txt"),
                    dest_rel: Some(rel_path("notas/informe.txt")),
                    cause: SyncFailureCause::IllegalName,
                    kind: SyncStepKind::Copy,
                },
            ],
            batch_id: Some(12),
            // Un `Mirror` que borra contra un destino con papelera que NOMBRA
            // lo que entierra: con esto en el informe, «¿se puede devolver este
            // lote?» se contesta sin haber guardado el `sync.plan_done` (#170).
            dest_trash: DestTrash::Restorable,
        },
    );
    check_one(
        fixtures,
        "sync_report_result_clean",
        &SyncReportResult {
            done: 3,
            failed: 0,
            skipped: 0,
            bytes: 4096,
            failures: vec![],
            batch_id: Some(12),
            // El contraste que hace útil el campo: tres copias limpias, y NADA
            // de esto vuelve. Sin `dest_trash` este informe y el de arriba son
            // el mismo informe para quien tenga que decidir si deshacer.
            dest_trash: DestTrash::Absent,
        },
    );
    check_one(
        fixtures,
        "sync_report_result_died",
        &SyncReportResult {
            done: 0,
            failed: 1,
            skipped: 0,
            bytes: 0,
            failures: vec![SyncFailure {
                rel: rel_path("a.txt"),
                dest_rel: None,
                cause: SyncFailureCause::Denied,
                kind: SyncStepKind::Copy,
            }],
            batch_id: None,
            // `Opaque`: hay papelera y no dice dónde deja las cosas. Que el
            // `batch_id` sea `None` no lo contradice — son dos preguntas, y con
            // lote ausente no hay nada que deshacer de todos modos.
            //
            // Esta fixture es de DECODE: `new_report` siempre abre el informe
            // con su lote puesto y es el único constructor del core, así que un
            // informe sin `batch_id` no lo produce este daemon. Se congela
            // porque el campo es opcional en el wire desde 0.40.0 y un cliente
            // tiene que saber leer la forma sin él (`protocol-guardian`, W4b
            // MINOR-6).
            dest_trash: DestTrash::Opaque,
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
            dest_anchor: None,
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
            dest_anchor: None,
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
            dest_anchor: None,
        },
    );
    // 0.54.0 (#295): el ancla del listado VOLVIENDO con la petición que
    // escribe. Las dos fixturas —copiar y mover— congelan que el campo se
    // llama igual en las dos y que se OMITE cuando no está: sin eso, un
    // cliente 0.53 y uno 0.54 sin ancla no producirían el mismo JSON.
    check_one(
        fixtures,
        "fs_copy_params_anchored",
        &FsCopyParams {
            from: vpath("file:///src/a.txt"),
            to: vpath("file:///dst/sub/a.txt"),
            on_collision: CollisionPolicy::Fail,
            symlinks: SymlinkPolicy::Preserve,
            resume: ResumePolicy::Off,
            verify: VerifyPolicy::Length,
            dest_anchor: Some(norte_proto::DirAnchor::new(
                "3f2a91c40b7d6e58aa10c4d9f8e37b62".to_owned(),
            )),
        },
    );
    check_one(
        fixtures,
        "fs_move_params_anchored",
        &FsMoveParams {
            from: vpath("file:///src/dir/a.txt"),
            to: vpath("file:///dst/sub/a.txt"),
            on_collision: CollisionPolicy::Fail,
            symlinks: SymlinkPolicy::Preserve,
            resume: ResumePolicy::Off,
            verify: VerifyPolicy::Length,
            dest_anchor: Some(norte_proto::DirAnchor::new(
                "3f2a91c40b7d6e58aa10c4d9f8e37b62".to_owned(),
            )),
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
        &DaemonShutdownParams {
            graceful: true,
            mode: ShutdownMode::Stop,
        },
    );
    check_one(
        fixtures,
        "daemon_shutdown_params_hard",
        &DaemonShutdownParams {
            graceful: false,
            mode: ShutdownMode::Stop,
        },
    );
    check_one(fixtures, "daemon_shutdown_result", &DaemonShutdownResult {});
    // 0.46.0: el relevo, y la notificación con la que se anuncia.
    check_one(
        fixtures,
        "daemon_shutdown_params_handover",
        &DaemonShutdownParams {
            graceful: true,
            mode: ShutdownMode::Handover,
        },
    );
    check_one(
        fixtures,
        "daemon_going_away",
        &DaemonGoingAway { reconnect: true },
    );
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
                unreadable: None,
                unvisited: None,
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
    check_methods_v05_capabilities(fixtures);
}

/// La segunda mitad de [`check_methods_v05`]: `fs.capabilities` y su catálogo
/// de atributos.
///
/// Partida en dos porque la primera pasó de cien líneas al ganar `unvisited`
/// (0.62.0), no porque sean dos familias: son la misma versión del wire.
fn check_methods_v05_capabilities(fixtures: &BTreeMap<String, Value>) {
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
// Una LISTA: un `assert_eq!` por nombre del wire, y crece con el vocabulario.
// Partirla en dos mitades arbitrarias escondería la mitad, y lo que hace útil
// una lista congelada es verla entera — mismo criterio que la tabla de
// `efecto_de` en la ventana.
#[expect(
    clippy::too_many_lines,
    reason = "un nombre por método, congelados de una vez"
)]
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
    assert_eq!(methods::PLUGIN_RENAME_PLAN, "plugin.rename_plan");
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
    assert_eq!(methods::FS_CREATE, "fs.create");
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
    // …y el informe del lote, que es lo que un `Failed` no puede contar.
    assert_eq!(methods::FS_RENAME_BATCH_REPORT, "fs.rename_batch_report");
    // Los LITERALES, no los símbolos, por el mismo motivo que
    // `PLUGIN_HELP_MAX_BYTES` arriba: un receptor dimensiona contra ellos —
    // rechaza el lote antes de mandarlo, reserva el buffer del hash — así que
    // moverlos mueve el contrato y algo tiene que ponerse rojo. El tope de
    // parejas RECHAZA (no recorta como `FS_LIST_MAX_PAGE`), y por eso importa
    // aún más que un tercero lo conozca.
    assert_eq!(methods::FS_RENAME_BATCH_MAX_PAIRS, 4096);
    assert_eq!(methods::PLAN_HASH_LEN, 64);
    // 0.37.0 (#131): host.volumes — enumeración de los volúmenes del host,
    // SOLO para una conexión User (diseño §C de `2026-08-10-volumes-design.md`).
    assert_eq!(methods::HOST_VOLUMES, "host.volumes");
    // 0.56.0 (#264): connection.list — las conexiones nombradas del daemon,
    // para que un frontend ofrezca un selector sin leer `connections.toml` él
    // mismo. SOLO `User`, por lo mismo que `host.volumes`.
    assert_eq!(methods::CONNECTION_LIST, "connection.list");
    // 0.38.0 (task V3.5 del plan de volúmenes): `Volume::label` pasa a
    // `Option<Vec<u8>>` — corrección de wire dentro de la misma rama sin
    // publicar, ventana desplazada igual que cualquier bump.
    // 0.39.0 (ADR 0048): fs.compare — la comparación de dos árboles como Task,
    // con sus filas por notificación.
    assert_eq!(methods::FS_COMPARE, "fs.compare");
    assert_eq!(methods::COMPARE_ROWS, "compare.rows");
    // Los LITERALES, no los símbolos, por el mismo motivo que
    // `FS_RENAME_BATCH_MAX_PAIRS` arriba: son contrato que un tercero
    // dimensiona por su cuenta. El tope de lote es el mismo que el de
    // `search.hits` A PROPÓSITO — un lote de filas no es más caro que uno de
    // hits —, y el de entradas por directorio es el que convierte un
    // directorio desmesurado en UNA fila de error en vez de un OOM.
    assert_eq!(methods::COMPARE_ROWS_MAX_BATCH, 256);
    assert_eq!(methods::COMPARE_MAX_DIR_ENTRIES, 200_000);
    // 0.40.0 (ADR 0049): sync.plan/apply/report — la sincronización de un
    // sentido como plan aprobable, retenido y deshacible.
    assert_eq!(methods::SYNC_PLAN, "sync.plan");
    assert_eq!(methods::SYNC_STEPS, "sync.steps");
    assert_eq!(methods::SYNC_PLAN_DONE, "sync.plan_done");
    assert_eq!(methods::SYNC_APPLY, "sync.apply");
    assert_eq!(methods::SYNC_REPORT, "sync.report");
    // Los LITERALES otra vez. El TTL es el único de los cuatro que no acota una
    // colección: es la ventana entre aprobar y ejecutar, y por tanto lo que el
    // ejecutor tiene que revalidar — un tercero que la dimensione mal deja
    // planes que caducan bajo el ratón. El tope de `include` RECHAZA (no
    // recorta), como el de parejas de renames.
    assert_eq!(methods::SYNC_STEPS_MAX_BATCH, 256);
    assert_eq!(methods::SYNC_PLAN_TTL_MS, 600_000);
    assert_eq!(methods::SYNC_MAX_BLOCKERS_REPORTED, 256);
    assert_eq!(methods::SYNC_MAX_INCLUDE, 4096);
    // 0.41.0 (#178): `Error::JournalUnavailable` — un journal ilegible rehúsa
    // la mutación en vez de dejarla pasar sin registro. Categoría nueva, así
    // que MINOR: un cliente 0.40.x la degrada a `Unknown`.
    //
    // 0.42.0 (#170, #152, #195): TRES campos en tipos que ya existían, un solo
    // bump — `SyncReportResult::dest_trash`, `SyncFailure::kind` y
    // `CompareRow::paired_under`. Ni método ni notificación nuevos, así que no
    // hay literal que añadir arriba; lo que cambia son las formas, y eso lo
    // pinean `methods.json` y `compare_row.json`.
    //
    // 0.43.0 (#171): `PolicyUndoReportResult` gana `denied`/`denied_total` — la
    // policy se pregunta unidad a unidad y DENTRO de la Task, así que una
    // denegación es una fila del informe en vez de parar el undo entero.
    // Campos nuevos con `#[serde(default)]`, así que un cliente 0.42.x los
    // ignora y ve el informe de siempre.
    //
    // 0.43.0 (#207): `SyncReason::NonInjectivePairing` — un plan ya no
    // sobrescribe una pareja que solo se sostiene sobre una transformación que
    // puede juntar ficheros distintos (el singleton NFC). Variante nueva de un
    // enum que degrada con `#[serde(other)]`, así que MINOR: un cliente 0.42.x
    // la lee como `Unknown` y pinta «un motivo que esta versión no sabe
    // nombrar» sobre un paso que YA es un `Skip` en el wire — no actúa de
    // menos ni de más.
    //
    // 0.44.0 (#182): `Error::LIMIT_RETAINED_SYNC_PLANS` — un token más del
    // vocabulario ABIERTO de `LimitExceeded`, para que el rechazo del tope de
    // planes retenidos viaje con taxonomía en vez de llegar como «internal
    // error». Un cliente N-1 lo enseña tal cual, que es el contrato del campo.
    // 0.45.0 (#145, #164, ADR 0054): `CapabilityFlags::FULL_FOLD` y
    // `CONFINED_WRITES`, más `ConflictKind::EscapesRoot`. Los dos flags son
    // nombres nuevos de un vocabulario que ADR 0004 obliga a IGNORAR cuando no
    // se conoce, y el subtipo de conflicto degrada a `Unknown` por el
    // `#[serde(other)]` de ADR 0005: un cliente 0.44.x lee «conflicto que esta
    // versión no sabe nombrar» sobre una operación que igualmente falló, no
    // actúa de más. MINOR, por tanto, y no MAJOR.
    assert_eq!(methods::DAEMON_GOING_AWAY, "daemon.going_away");
    // 0.46.0 (roadmap ítem 10): `daemon.going_away` y `DaemonShutdownParams.mode`.
    // Los dos son ADITIVOS y ninguno cambia lo que ya se emitía: `mode` no se
    // serializa cuando vale `Stop`, así que una parada corriente de un cliente
    // 0.46 es byte por byte el mensaje de 0.45; y una notificación que un
    // cliente viejo no conoce se ignora, que es lo que ADR 0004 le obliga a
    // hacer — se queda sin saber que venía un relevo y reconecta como siempre,
    // que es exactamente el comportamiento de hoy. MINOR.
    //
    // `ShutdownMode` NO lleva `#[serde(other)]`, contra la costumbre de este
    // wire: degradar está bien cuando malinterpretar un valor cuesta una
    // feature, y mal cuando apaga un daemon de una forma que nadie pidió.
    // 0.47.0 (roadmap ítem 11): `rar` en `ARCHIVE_FORMATS`. Ampliar la
    // whitelist no cambia ningún mensaje: cambia qué schemes compuestos se
    // pueden FORMAR. Un cliente 0.46 no los forma y no ve la funcionalidad;
    // uno 0.47 contra un daemon 0.46 no llega a intentarlo, porque
    // `version_compatible` no negocia un minor de cliente MAYOR que el del
    // servidor. MINOR.
    assert!(norte_proto::ARCHIVE_FORMATS.contains(&"rar"));
    // 0.48.0 (L2): `session.get`/`session.put` y sus cuatro tipos, más
    // `ConflictKind::StaleRevision` y el token `Error::LIMIT_SESSION_BODY`.
    // Aditivo: no toca un solo mensaje existente, y el cuerpo de la sesión es
    // OPACO —el wire congela que viaja tal cual, no qué lleva dentro—. El
    // subtipo degrada a `Unknown` por el `#[serde(other)]` de ADR 0005 y el
    // token de límite es vocabulario ABIERTO que un cliente N-1 enseña tal
    // cual: los dos dejan al cliente viejo con la conducta correcta —volver a
    // leer, y no reintentar el mismo cuerpo—. MINOR.
    assert_eq!(norte_proto::Error::LIMIT_SESSION_BODY, "session-body");
    // Los NOMBRES, como los de todas las demás familias: renombrar un método
    // es un cambio de wire, y el doctest que los enseña no es el sitio donde
    // este test dice que lo mira.
    assert_eq!(methods::SESSION_GET, "session.get");
    assert_eq!(methods::SESSION_PUT, "session.put");
    // 0.49.0: `fs.dir_size` con su `TaskKind::DirSize` (#139) y
    // `connection.close` (#140). Aditivo — métodos nuevos que un cliente viejo
    // no forma, y una variante de kind que su `#[serde(other)]` degrada a
    // `Unknown` desde 0.10. Los dos van en el MISMO bump a propósito: la
    // ventana se mueve una vez por release del wire, y esta rama no ha salido.
    // MINOR.
    assert_eq!(methods::FS_DIR_SIZE, "fs.dir_size");
    assert_eq!(methods::FS_CHECKSUM, "fs.checksum");
    assert_eq!(methods::FS_CHECKSUM_REPORT, "fs.checksum_report");
    assert_eq!(methods::FS_CHECKSUM_MAX_PATHS, 4096);
    // 0.60.0 (#314). El golden del payload se indexa por el nombre de la
    // FIXTURA, no por esta constante, así que sin estas dos líneas renombrar
    // el método pasaba la suite entera — que es justo lo que este test existe
    // para impedir. `MODE_PERMISSION_BITS` es contrato de validación citado en
    // la rustdoc del campo: un cliente dimensiona contra él.
    assert_eq!(methods::FS_SET_MODE, "fs.set_mode");
    assert_eq!(methods::FS_SET_MODE_MAX_PATHS, 4096);
    assert_eq!(methods::MODE_PERMISSION_BITS, 0o7777);
    // 0.62.0 (#315, #121). Los dos topes son contrato como los de arriba, y el
    // primero además es de otra clase: los demás RECHAZAN por encima de su
    // número y éste TRUNCA, así que lo que el cliente necesita para no leerlo
    // mal es la señal (`TaskProgress::unvisited`), no el número.
    assert_eq!(methods::SET_MODE_RECURSIVE_MAX, 100_000);
    assert_eq!(methods::AI_RENAME_NAMES_MAX, 4096);
    assert_eq!(methods::CONNECTION_CLOSE, "connection.close");
    // 0.50.0: escribir archivos (#132). Cuatro métodos y cuatro kinds nuevos,
    // aditivos por la misma razón que los de arriba. Ninguno escribe DENTRO de
    // un contenedor —el provider de archivos sigue `READ_ONLY`, ADR 0018—: los
    // cuatro fabrican ficheros nuevos. Desempaquetar no aparece porque no
    // necesita método: es un `fs.copy` desde el interior, que ya existía.
    // MINOR.
    assert_eq!(methods::ARCHIVE_PACK, "archive.pack");
    assert_eq!(methods::ARCHIVE_TEST, "archive.test");
    assert_eq!(methods::FILE_SPLIT, "file.split");
    assert_eq!(methods::FILE_COMBINE, "file.combine");
    // El QUINTO: sin esta línea, renombrar `archive.test_report` pasaba la
    // suite entera. Es el método por el que se recoge qué entrada está
    // corrupta, así que su nombre es contrato igual que los otros cuatro.
    assert_eq!(methods::ARCHIVE_TEST_REPORT, "archive.test_report");
    // Y el SEXTO, por lo mismo (#250): los goldens congelan la forma del
    // payload, no la cadena del método, y tanto el brazo del dispatch como el
    // SDK citan la constante — así que se mueven juntos y renombrarla pasaba
    // la suite entera.
    assert_eq!(methods::ARCHIVE_PACK_REPORT, "archive.pack_report");
    // Los topes que un cliente puede enseñar ANTES de mandar nada: 999 trozos
    // es la convención `.001`, y descubrirlo en el trozo 1000 dejaría un
    // conjunto que nadie puede volver a juntar.
    assert_eq!(methods::FILE_SPLIT_MAX_PARTS, 999);
    assert_eq!(methods::FILE_SPLIT_MIN_BYTES, 4096);
    assert_eq!(methods::ARCHIVE_TEST_MAX_FAILURES, 256);
    // 0.51.0 (#247): ni un tipo ni un campo nuevos — lo que cambió es lo que
    // `session.put` ACEPTA (un esquema que este core no sabe leer se rehúsa,
    // en vez de escribirse y matar la persistencia desde el arranque
    // siguiente). Un bump por comportamiento del wire, que también cuenta.
    // 0.53.0 (#251, #265, #282): tres campos OPCIONALES —el recuento de
    // ilegibles de una task, los bytes del directorio de un plugin roto y el
    // ancla que un humano leyó al aprobar—. Los tres se omiten cuando no hay
    // nada que decir, así que el JSON de un caso corriente no cambia; lo que
    // desplaza la ventana es que un peer viejo no puede hacer la comprobación
    // que cada uno habilita.
    // 0.54.0 (#295): dos campos OPCIONALES que son el mismo dato en los dos
    // sentidos —la identidad opaca del directorio que un listado devolvió, y
    // la que la copia o el movimiento devuelven para decir «era ese»—. Se
    // omiten cuando no hay nada que decir, así que el JSON corriente no
    // cambia; lo que desplaza la ventana es que un peer 0.53 no puede hacer
    // la comprobación que habilitan.
    // 0.55.0 (#279): `Error::ApprovalGone`, que dice CUÁL de las tres formas
    // de «esa aprobación ya no está» ocurrió. Aditivo —`Error` es
    // `#[non_exhaustive]` y una categoría desconocida cae en `Unknown`—, así
    // que lo que desplaza la ventana no es el JSON sino que un peer 0.54
    // seguirá contando las tres como un error genérico.
    // 0.56.0 (#264): `connection.list`. Aditivo —un método que un cliente
    // viejo no llama—, y aun así la ventana se DESPLAZA: contra un daemon
    // 0.55 no hay selector de conexiones. Lo que no se pierde es conectar,
    // que sigue siendo navegar a una URL.
    // 0.57.0 (#290): `fs.create`, un fichero VACÍO como Task. Aditivo —método
    // nuevo, kind nuevo que degrada a `Unknown`—, y desplaza la ventana porque
    // contra un daemon 0.56 un frontend sin terminal no puede ofrecer «editar
    // uno nuevo»: no hay forma de crear el fichero.
    // 0.58.0 (#250): `archive.pack_report`, qué guardó ese empaquetado que
    // SIGNIFICA otra cosa fuera. Aditivo —método nuevo que un cliente viejo no
    // llama— y la ventana se desplaza en la dirección de 0.51.0: un cliente
    // 0.57 contra un daemon 0.58 empaqueta igual y se queda sin el aviso. (Las
    // colisiones por plegado no entran en este informe: esas se RECHAZAN al
    // empaquetar, porque ahí sí desaparece un fichero al extraer.)
    // 0.59.0 (#311): `fs.checksum` y su informe. Aditivo —dos métodos que un
    // cliente viejo no llama y un kind que degrada a `Unknown`— y aquí no hay
    // degradación parcial ninguna: contra un daemon 0.58 no se puede
    // comprobar una suma en absoluto, que es lo que desplaza la ventana.
    // 0.60.0 (#314): `fs.set_mode`, con su kind y la capability `POSIX_MODE`.
    // Aditivo, y la ventana se desplaza porque contra un daemon 0.59 no se
    // pueden cambiar permisos: la superficie de propiedades sigue siendo de
    // solo mirar, que es lo que era antes de esta versión.
    // 0.61.0 (#314): `ApprovalDetail`, y con él el `detail` de las dos formas
    // de una aprobación. Aditivo —se omite cuando no dice nada— y la ventana se
    // desplaza porque contra un daemon 0.60 la pregunta de un `set-mode` no
    // puede decir QUÉ modo, que es la mitad de esa decisión.
    // 0.62.0 (#315, #121): `recursive`/`dir_mode` en `fs.set_mode` y `names`
    // en `ai.rename_plan`. Los tres campos son ALCANCE —sobre qué actúa una
    // petición— y los tres se omiten cuando no dicen nada, así que el JSON de
    // un cliente que no los manda no cambia ni un byte. La ventana se desplaza
    // porque contra un daemon 0.61 no se puede pedir ninguna de las dos cosas:
    // los permisos se cambian ruta a ruta y el plan de la IA es del directorio
    // entero.
    // 0.63.0 (#325): `Error::SecretNeeded` y `connection.provide_secret`. La
    // pregunta que el core no puede hacer por su cuenta —su resolver de
    // secretos no tiene interfaz de usuario ni debe tenerla— subiendo por el
    // cable para que la conteste quien está delante, con el mismo flujo que el
    // TOFU de las host keys. Un cliente 0.62 degrada el error a `Unknown` y
    // enseña un fallo donde el nuevo abre un diálogo: es lo que ya hacía.
    assert_eq!(
        methods::CONNECTION_PROVIDE_SECRET,
        "connection.provide_secret"
    );
    // 0.64.0 (#322): `connection.failed`. El fallo de conexión llegaba como
    // categoría —`PermissionDenied`, indistinguible de una clave equivocada— y
    // la frase que lo explicaba moría en el log del daemon; con la CLI
    // embebida sí se leía, o sea que el diagnóstico dependía del TRANSPORTE.
    // Va por notificación porque la taxonomía no lleva texto libre a
    // propósito: con el error se decide, y se decide por categoría. Un cliente
    // 0.63 la descarta y se queda como estaba.
    assert_eq!(methods::CONNECTION_FAILED, "connection.failed");
    // 0.65.0 (#328): `log.tail` y `log.level`. El registro del DAEMON, que un
    // frontend con proceso aparte no puede ver de ninguna otra forma — su
    // panel pinta el anillo del proceso equivocado, y desde #326 lo dice. Se
    // TIRA con un cursor y no se empuja: el daemon no guarda estado por
    // cliente y la respuesta dice cuántas líneas se cayeron por detrás, que es
    // lo que una notificación perdida no puede decir. Y subir el nivel es un
    // MÉTODO para que la cota que impide enseñar un `PASS` de FTP la aplique
    // el único código que puede aplicarla: el que tiene el anillo.
    assert_eq!(methods::LOG_TAIL, "log.tail");
    assert_eq!(methods::LOG_LEVEL, "log.level");
    // 0.66.0 (D4): ningún método nuevo — dos campos opcionales, `SpanWire::bg`
    // y `PluginPreviewStyledParams::columns`, para el previewer de imagen que
    // pinta medios bloques y necesita saber a cuántas celdas encoger.
    // 0.69.0 (ADR 0100): `plugin.notice`, la notificación con la que un
    // plugin `hook` le dice algo al humano sobre una mutación ya registrada —
    // o con la que el daemon dice que apagó los hooks de un plugin. Solo a
    // humanos, como `connection.failed`; un cliente 0.68 la descarta.
    assert_eq!(methods::PLUGIN_NOTICE, "plugin.notice");
    assert_eq!(norte_proto::PROTOCOL_VERSION, "0.69.0");
}

/// Una [`Entry`] de fila de comparación: los cuatro campos que el panel pinta,
/// sin atributos (se omiten vacíos).
fn compare_entry(wire: &str, kind: EntryKind, size: Option<u64>, mtime_ms: Option<i64>) -> Entry {
    Entry {
        path: vpath(wire),
        kind,
        size,
        mtime_ms,
        attrs: BTreeMap::new(),
    }
}

/// Una fila sin los tres campos opcionales; quien necesite alguno la completa
/// con sintaxis de actualización de struct.
fn compare_row(
    id: u64,
    left: Option<Entry>,
    right: Option<Entry>,
    verdict: norte_proto::methods::CompareVerdict,
    criterion: norte_proto::methods::CompareCriterion,
    confidence: norte_proto::methods::CompareConfidence,
) -> norte_proto::methods::CompareRow {
    norte_proto::methods::CompareRow {
        id,
        left,
        right,
        verdict,
        criterion,
        confidence,
        newer: None,
        reason: None,
        side: None,
        paired_under: None,
    }
}

/// La FILA de `fs.compare` (0.39.0, ADR 0048), congelada: una fixture por
/// veredicto y, entre todas, el vocabulario ENTERO que el core llega a emitir
/// — los seis criterios, las tres confianzas, los cinco motivos, los dos
/// lados y, desde 0.42.0, las tres transformaciones de emparejamiento
/// ([`compare_row_cases_paired`]). Los cinco fallbacks de `#[serde(other)]` NO
/// tienen fixture a propósito: el core jamás los emite, así que no hay
/// dirección de encode que pinear, y su degradación en DECODE la cubre
/// `types.rs` (`unknown_enum_tokens_degrade_and_do_not_error`).
///
/// Lo que estas fixtures pinean, campo a campo:
///
/// - `same_size_unknown` es la razón de ser de la confianza: el lado derecho
///   está DENTRO de un zip (`zip+file://…/!/…`, ADR 0018), que no da tamaño ni
///   fecha fiables. La respuesta es `Same`/`Unknown`, no un error y no un
///   `Certain` inventado.
/// - `only_left_hostile` (izquierda) y las dos `ambiguous_case_fold_hostile*`
///   llevan nombres no-UTF8 (`%FF`, `%FE`): regla dura 1 en las dos
///   direcciones, y en los DOS lados de la comparación
///   (`ambiguous_normalization_right` es del lado derecho).
/// - Las tres filas `ambiguous_*` congelan la FORMA que el rustdoc de
///   `CompareVerdict::Ambiguous` declara normativa: una colisión es de UN
///   lado, así que es UNA fila por entrada implicada, con el otro lado en
///   `None`. Las dos del par `case_fold` son las dos entradas que colapsan,
///   con TAMAÑOS DISTINTOS para que se vea que son dos entradas y no una
///   contada dos veces.
/// - `ambiguous_normalization_right` escribe el gemelo NFD con escapes para
///   que ningún editor pueda normalizarlo por su cuenta — mismo cuidado que
///   `rename_collision.json`.
/// - `error_dir_too_large_right` NO lleva entrada de ningún lado: un
///   directorio que se pasa del tope se reporta sin haber podido listar nada,
///   y esa es justo la forma que exime [`CompareRow::sides_are_consistent`].
/// - Los `criterion` de las filas de problema (`ambiguous`, `error`) son
///   `presence` salvo cuando un rung concreto sí llegó a correr
///   (`error_read_failed_left`, que muere DENTRO del hash). El tipo no tiene
///   variante «ningún rung» y no se le inventa una aquí: `unknown` es el
///   fallback de decode y el core no lo emite jamás.
/// - Las QUINCE filas de 0.39.0 no llevan `paired_under` y su JSON no cambió ni
///   un byte al añadirlo (0.42.0): la clave se omite cuando no hay
///   transformación que nombrar, que es el caso corriente. Eso es lo que hace
///   ADITIVO el campo, y este fichero es donde se ve.
#[test]
fn golden_compare_row() {
    let mut cases = compare_row_cases_content();
    cases.extend(compare_row_cases_presence());
    cases.extend(compare_row_cases_problems());
    cases.extend(compare_row_cases_paired());

    // Toda fixture congelada tiene que ser una fila LEGAL: si un golden dijera
    // `OnlyLeft` llevando lado derecho, congelaría el bug en vez del contrato,
    // y el core de la task C6 se escribiría contra él.
    for (name, row) in &cases {
        assert!(row.sides_are_consistent(), "[compare_row/{name}] lados");
        assert!(row.reason_is_consistent(), "[compare_row/{name}] motivo");
    }

    check_family("compare_row.json", &cases);
}

/// Las parejas cuyos dos nombres NO son los mismos bytes (0.42.0, #152): el
/// veredicto es normal —`Same` o `Different`, decidido por el rung que tocara—
/// y lo que congela cada fixture es [`CompareRow::paired_under`], que es lo
/// único que dice que las dos mitades se deletrean distinto.
///
/// Las tres, y no una: separar el singleton de las otras dos ES el contrato.
/// Un consumidor que solo viera «difieren en bytes» tendría que elegir entre
/// fiarse de todo emparejamiento por normalización —el bug de #152— o
/// rechazarlos todos, que rompe el caso macOS↔Linux para el que la clave
/// existe.
///
/// El KELVIN SIGN y el gemelo NFD van escritos con escapes `\u`, por lo mismo
/// que `ambiguous_normalization_right`: un editor que normalizara el fichero
/// convertiría estos tests en tautologías.
fn compare_row_cases_paired() -> Vec<(&'static str, norte_proto::methods::CompareRow)> {
    use norte_proto::methods::CompareConfidence as Conf;
    use norte_proto::methods::CompareCriterion as Crit;
    use norte_proto::methods::CompareVerdict as V;
    use norte_proto::methods::{CompareRow, PairTransform};
    vec![
        // Un lado no distingue caja, así que los dos nombres NO pueden
        // coexistir allí y emparejarlos es lo correcto. Viaja para que un
        // pintor pueda explicar por qué la fila enseña dos grafías.
        (
            "same_paired_under_case_fold",
            CompareRow {
                paired_under: Some(PairTransform::CaseFold),
                ..compare_row(
                    16,
                    Some(cf("file:///l/LEEME.txt", Some(7), Some(COMPARE_T))),
                    Some(cf("file:///r/leeme.txt", Some(7), Some(COMPARE_T))),
                    V::Same,
                    Crit::Hash,
                    Conf::Certain,
                )
            },
        ),
        // El caso para el que se diseñó la clave: el MISMO texto repartido en
        // NFC por Linux y en NFD por macOS. Tampoco es un aviso; lo que importa
        // al escribir es que el destino se deletrea de otra manera.
        (
            "same_paired_under_normalization",
            CompareRow {
                paired_under: Some(PairTransform::Normalization),
                ..compare_row(
                    17,
                    Some(cf("file:///l/caf\u{e9}.txt", Some(3), Some(COMPARE_T))),
                    Some(cf("file:///r/cafe\u{301}.txt", Some(3), Some(COMPARE_T))),
                    V::Same,
                    Crit::Hash,
                    Conf::Certain,
                )
            },
        ),
        // #145 en una fila: `straße.txt` contra `strasse.txt`. Los junta el
        // pliegue COMPLETO de un ext4/f2fs `+F` y nada más — en cualquier otro
        // volumen son dos ficheros, y pueden ser dos ficheros distintos. Por
        // eso NO comparte variante con `case_fold`, cuya promesa es «un mismo
        // texto escrito de dos maneras».
        (
            "different_paired_under_full_fold",
            CompareRow {
                paired_under: Some(PairTransform::FullFold),
                ..compare_row(
                    19,
                    Some(cf("file:///l/stra\u{df}e.txt", Some(11), Some(COMPARE_T))),
                    Some(cf("file:///r/strasse.txt", Some(22), Some(COMPARE_T))),
                    V::Different,
                    Crit::Size,
                    Conf::Certain,
                )
            },
        ),
        // #152 entero en una fila: U+212A KELVIN SIGN contra la `K` ASCII.
        // Coexisten en ext4, se leen como caracteres DISTINTOS, y sin esta
        // marca un plan de sincronización lee este `Different` como «actualiza
        // el de la derecha con el de la izquierda» y escribe encima de un
        // fichero que no tiene nada que ver. El veredicto y el criterio son los
        // normales: lo anómalo no es la comparación, es la PAREJA.
        (
            "different_paired_under_singleton",
            CompareRow {
                paired_under: Some(PairTransform::NormalizationSingleton),
                ..compare_row(
                    18,
                    Some(cf("file:///l/\u{212a}.txt", Some(10), Some(COMPARE_T))),
                    Some(cf("file:///r/K.txt", Some(20), Some(COMPARE_T))),
                    V::Different,
                    Crit::Size,
                    Conf::Certain,
                )
            },
        ),
    ]
}

/// La fecha de referencia de las fixtures de comparación.
const COMPARE_T: i64 = 1_720_000_000_000;

fn cf(wire: &str, size: Option<u64>, mtime_ms: Option<i64>) -> Entry {
    compare_entry(wire, EntryKind::File, size, mtime_ms)
}

fn cd(wire: &str) -> Entry {
    compare_entry(wire, EntryKind::Dir, None, None)
}

fn cln(wire: &str) -> Entry {
    compare_entry(wire, EntryKind::Symlink, None, None)
}

/// Las filas que decide un rung de CONTENIDO (hash, mtime, size,
/// `link_target`): las tres confianzas del vocabulario salen de aquí, porque es
/// aquí donde un criterio prueba, sugiere o no puede decir. Ver el rustdoc de
/// [`golden_compare_row`].
fn compare_row_cases_content() -> Vec<(&'static str, norte_proto::methods::CompareRow)> {
    use norte_proto::methods::CompareConfidence as Conf;
    use norte_proto::methods::CompareCriterion as Crit;
    use norte_proto::methods::CompareVerdict as V;
    use norte_proto::methods::{CompareRow, Side};
    vec![
        (
            "same_by_hash",
            compare_row(
                1,
                Some(cf("file:///l/a.bin", Some(4096), Some(COMPARE_T))),
                Some(cf("file:///r/a.bin", Some(4096), Some(COMPARE_T))),
                V::Same,
                Crit::Hash,
                Conf::Certain,
            ),
        ),
        // Dentro de la tolerancia por defecto (2000 ms): `Same`, pero solo
        // `Probable` — dos fechas parecidas no prueban dos ficheros iguales.
        (
            "same_by_mtime",
            compare_row(
                2,
                Some(cf("file:///l/doc.txt", Some(1234), Some(COMPARE_T))),
                Some(cf("file:///r/doc.txt", Some(1234), Some(COMPARE_T + 1_500))),
                V::Same,
                Crit::Mtime,
                Conf::Probable,
            ),
        ),
        (
            "same_size_unknown",
            compare_row(
                3,
                Some(cf("file:///l/leeme.txt", Some(7), Some(COMPARE_T))),
                Some(cf("zip+file:///r/paquete.zip/!/leeme.txt", None, None)),
                V::Same,
                Crit::Size,
                Conf::Unknown,
            ),
        ),
        (
            "different_by_size",
            compare_row(
                4,
                Some(cf("file:///l/informe.dat", Some(10), Some(COMPARE_T))),
                Some(cf("file:///r/informe.dat", Some(20), Some(COMPARE_T))),
                V::Different,
                Crit::Size,
                Conf::Certain,
            ),
        ),
        // El campo que esta spec NO lee y la 2 necesita: qué lado es más
        // nuevo. Se produce aquí porque no cuesta nada.
        (
            "different_by_mtime_newer_right",
            CompareRow {
                newer: Some(Side::Right),
                ..compare_row(
                    5,
                    Some(cf("file:///l/notas.md", Some(1234), Some(COMPARE_T))),
                    Some(cf(
                        "file:///r/notas.md",
                        Some(1234),
                        Some(COMPARE_T + 9_000),
                    )),
                    V::Different,
                    Crit::Mtime,
                    Conf::Probable,
                )
            },
        ),
        // Los symlinks se comparan, no se siguen: el destino es bytes.
        (
            "different_by_link_target",
            compare_row(
                6,
                Some(cln("file:///l/enlace")),
                Some(cln("file:///r/enlace")),
                V::Different,
                Crit::LinkTarget,
                Conf::Certain,
            ),
        ),
    ]
}

/// Las filas que decide la PRESENCIA o el tipo, antes de mirar contenido
/// alguno: siempre `Certain`, porque un lado que no existe no admite matices.
fn compare_row_cases_presence() -> Vec<(&'static str, norte_proto::methods::CompareRow)> {
    use norte_proto::methods::CompareConfidence as Conf;
    use norte_proto::methods::CompareCriterion as Crit;
    use norte_proto::methods::CompareVerdict as V;
    vec![
        (
            "only_left_hostile",
            compare_row(
                7,
                Some(cf("file:///l/informe%FF%FE.dat", Some(0), None)),
                None,
                V::OnlyLeft,
                Crit::Presence,
                Conf::Certain,
            ),
        ),
        // Un directorio huérfano es UNA fila y no se enumera: el plan de la
        // spec 2 lo copiará con un `fs.copy` recursivo.
        (
            "only_right_dir",
            compare_row(
                8,
                None,
                Some(cd("file:///r/fotos")),
                V::OnlyRight,
                Crit::Presence,
                Conf::Certain,
            ),
        ),
        (
            "type_mismatch",
            compare_row(
                9,
                Some(cf("file:///l/data", Some(5), Some(COMPARE_T))),
                Some(cd("file:///r/data")),
                V::TypeMismatch,
                Crit::Kind,
                Conf::Certain,
            ),
        ),
    ]
}

/// Las filas de PROBLEMA: las dos que llevan `reason` —`ambiguous` y
/// `error`— y, con ellas, los cinco motivos del vocabulario cerrado.
fn compare_row_cases_problems() -> Vec<(&'static str, norte_proto::methods::CompareRow)> {
    use norte_proto::methods::CompareConfidence as Conf;
    use norte_proto::methods::CompareCriterion as Crit;
    use norte_proto::methods::CompareReason as Why;
    use norte_proto::methods::CompareVerdict as V;
    use norte_proto::methods::{CompareRow, Side};
    vec![
        // Una colisión es de UN lado, así que la fila también: UNA fila por
        // entrada implicada, con esa entrada en el campo de SU lado y el otro
        // en `None`. `LEEME%FF.txt` y `leeme%FF.txt` son DOS filas —con
        // tamaños distintos, para que se vea que no son la misma entrada
        // contada dos veces— porque un plan de sincronización tiene que ver
        // los DOS nombres antes de escribir sobre cualquiera de ellos. La
        // forma está congelada en el rustdoc de `CompareVerdict::Ambiguous`
        // (hallazgo MAJOR de protocol-guardian: estas fixtures llevaban una
        // pareja cruzada, que es justo lo que el diseño dice que NO es una
        // ambigüedad).
        (
            "ambiguous_case_fold_hostile",
            CompareRow {
                reason: Some(Why::CaseFold),
                side: Some(Side::Left),
                ..compare_row(
                    10,
                    Some(cf("file:///l/LEEME%FF.txt", Some(12), Some(COMPARE_T))),
                    None,
                    V::Ambiguous,
                    Crit::Presence,
                    Conf::Unknown,
                )
            },
        ),
        (
            "ambiguous_case_fold_hostile_twin",
            CompareRow {
                reason: Some(Why::CaseFold),
                side: Some(Side::Left),
                ..compare_row(
                    11,
                    Some(cf("file:///l/leeme%FF.txt", Some(34), Some(COMPARE_T))),
                    None,
                    V::Ambiguous,
                    Crit::Presence,
                    Conf::Unknown,
                )
            },
        ),
        // La colisión por normalización, y en el lado DERECHO: el gemelo NFD
        // de un nombre que ese mismo directorio ya tiene en NFC. Va escrito
        // con escapes para que ningún editor lo normalice por su cuenta —
        // mismo cuidado que `rename_collision.json`—, y su gemelo NFC tiene su
        // propia fila exactamente igual que la pareja de arriba.
        (
            "ambiguous_normalization_right",
            CompareRow {
                reason: Some(Why::Normalization),
                side: Some(Side::Right),
                ..compare_row(
                    12,
                    None,
                    Some(cf("file:///r/cafe\u{301}.txt", Some(3), Some(COMPARE_T))),
                    V::Ambiguous,
                    Crit::Presence,
                    Conf::Unknown,
                )
            },
        ),
        (
            "error_unreadable_left",
            CompareRow {
                reason: Some(Why::Unreadable),
                side: Some(Side::Left),
                ..compare_row(
                    13,
                    Some(cd("file:///l/denegado")),
                    Some(cd("file:///r/denegado")),
                    V::Error,
                    Crit::Presence,
                    Conf::Unknown,
                )
            },
        ),
        (
            "error_dir_too_large_right",
            CompareRow {
                reason: Some(Why::DirTooLarge),
                side: Some(Side::Right),
                ..compare_row(14, None, None, V::Error, Crit::Presence, Conf::Unknown)
            },
        ),
        (
            "error_read_failed_left",
            CompareRow {
                reason: Some(Why::ReadFailed),
                side: Some(Side::Left),
                ..compare_row(
                    15,
                    Some(cf("file:///l/grande.bin", Some(4096), Some(COMPARE_T))),
                    Some(cf("file:///r/grande.bin", Some(4096), Some(COMPARE_T))),
                    V::Error,
                    Crit::Hash,
                    Conf::Unknown,
                )
            },
        ),
    ]
}

/// Un paso sin los tres campos opcionales; quien necesite alguno la completa
/// con sintaxis de actualización de struct.
fn sync_step(
    id: u64,
    kind: norte_proto::methods::SyncStepKind,
    rel: &str,
    criterion: norte_proto::methods::CompareCriterion,
    confidence: norte_proto::methods::CompareConfidence,
    reversal: Option<norte_proto::methods::StepReversal>,
) -> norte_proto::methods::SyncStep {
    norte_proto::methods::SyncStep {
        id,
        kind,
        rel: rel_path(rel),
        dest_rel: None,
        size: None,
        criterion,
        confidence,
        reversal,
        reason: None,
    }
}

/// El PASO de `sync.plan` (0.40.0, ADR 0049), congelado: una fixture por clase
/// de paso y, entre todas, el vocabulario ENTERO que el core llega a emitir —
/// las cinco clases, las tres reversas, los cuatro motivos y las tres
/// confianzas. Los fallbacks de `#[serde(other)]` NO tienen fixture a
/// propósito, por el mismo motivo que en `compare_row.json`: el core jamás los
/// emite, así que no hay dirección de encode que pinear, y su degradación en
/// DECODE la cubre `types.rs`.
///
/// Lo que estas fixtures pinean, campo a campo:
///
/// - Las tres parejas `*_trash` / `*_irreversible` son la razón de ser de
///   [`StepReversal`]: el MISMO paso vale `restore_trash` o `irreversible`
///   según si el DESTINO tiene papelera, y en el segundo caso debe una razón.
///   Un plan que no supiera distinguirlos le prometería a un humano un undo
///   que no existe.
/// - `copy_hostile` lleva un `rel` no-UTF8 (`%FF%FE`): regla dura 1 en las dos
///   direcciones. El `rel` es RELATIVO a las dos raíces, así que no lleva
///   ninguna de ellas.
/// - `overwrite_dest_spelt_differently` es la pareja que la clave de
///   emparejamiento junta y los bytes separan: el paso lleva los DOS caminos,
///   porque se lee del que el origen deletrea y se escribe sobre el que el
///   destino tiene. Congela dos cosas — que `dest_rel` viaja SOLO cuando
///   difiere (en las otras diez fixtures la clave no aparece) y que lo que se
///   compara es la ruta ENTERA, no el último segmento.
/// - `overwrite_unknown_confidence` es el default `on_unknown: copy`: se
///   escribe, y el paso CONSERVA `confidence: unknown` para que el informe
///   pueda decir que copió porque nadie pudo asegurar nada.
///   `skip_unknown_confidence` es el mismo caso con la otra elección.
/// - Ningún `skip` lleva `reversal`, y todos llevan `reason`: es la invariante
///   que `shape_is_consistent` enuncia, y aquí está congelada como forma.
/// - `delete_tree` no lleva `size`: un borrado no mueve bytes, y la clave
///   ausente lo dice mejor que un cero.
#[test]
fn golden_sync_step() {
    let mut cases = sync_step_cases_acting();
    cases.extend(sync_step_cases_deleting());
    cases.extend(sync_step_cases_skipped());

    // Toda fixture congelada tiene que ser un paso LEGAL: uno que prometiera
    // una reversa imposible congelaría el bug en vez del contrato, y el
    // transductor de `norte-sync` se escribiría contra él.
    for (name, step) in &cases {
        assert!(step.shape_is_consistent(), "[sync_step/{name}] forma");
    }

    check_family("sync_step.json", &cases);
}

/// Los pasos que ESCRIBEN: crear, copiar y sobrescribir, con sus reversas.
fn sync_step_cases_acting() -> Vec<(&'static str, norte_proto::methods::SyncStep)> {
    use norte_proto::methods::{
        CompareConfidence as Conf, CompareCriterion as Crit, StepReversal as Rev,
        SyncReason as Why, SyncStep, SyncStepKind as Kind,
    };
    vec![
        (
            "create_dir",
            sync_step(
                1,
                Kind::CreateDir,
                "sub",
                Crit::Presence,
                Conf::Certain,
                Some(Rev::Delete),
            ),
        ),
        (
            "copy_hostile",
            SyncStep {
                size: Some(1234),
                ..sync_step(
                    2,
                    Kind::Copy,
                    "sub/informe%FF%FE.dat",
                    Crit::Presence,
                    Conf::Certain,
                    Some(Rev::Delete),
                )
            },
        ),
        (
            "overwrite_trash",
            SyncStep {
                size: Some(4096),
                ..sync_step(
                    3,
                    Kind::Overwrite,
                    "notas.md",
                    Crit::Mtime,
                    Conf::Probable,
                    Some(Rev::RestoreTrash),
                )
            },
        ),
        (
            // La MISMA fila que `overwrite_trash` —mismo rung, misma confianza,
            // mismo tamaño— contra un destino SIN papelera. Que la pareja no
            // varíe en nada más es lo que la convierte en una A/B de la
            // capacidad en vez de en dos ejemplos sueltos.
            "overwrite_irreversible",
            SyncStep {
                size: Some(4096),
                reason: Some(Why::NoTrashOnTarget),
                ..sync_step(
                    4,
                    Kind::Overwrite,
                    "notas.md",
                    Crit::Mtime,
                    Conf::Probable,
                    Some(Rev::Irreversible),
                )
            },
        ),
        (
            "overwrite_unknown_confidence",
            SyncStep {
                size: Some(7),
                ..sync_step(
                    5,
                    Kind::Overwrite,
                    "empaquetado/dentro.txt",
                    Crit::Mtime,
                    Conf::Unknown,
                    Some(Rev::RestoreTrash),
                )
            },
        ),
        (
            // La pareja que la clave de emparejamiento junta y el wire tenía
            // que poder nombrar: el origen deletrea el directorio `NOTAS` y el
            // destino —que no distingue caja— lo tiene como `notas`. El paso
            // lleva LOS DOS caminos; sin `dest_rel` el ejecutor escribiría bajo
            // el del origen y crearía un segundo directorio al lado.
            //
            // La diferencia va en un ANCESTRO y no en el último segmento, y es
            // deliberado por dos motivos. Uno: congela la regla que este campo
            // implementa de verdad —se compara la ruta ENTERA, porque la clave
            // pliega en cada nivel—. Dos: la hoja lleva el byte 0xFF, y un
            // nombre que no es UTF-8 NO se pliega (`key_for` lo devuelve crudo,
            // para no estropear los bytes de cola de Shift-JIS), así que una
            // pareja que solo difiriera en la caja de una hoja no-UTF8 no
            // existe: ningún walk la produce.
            //
            // La otra mitad del caso —NFC contra NFD— no se congela AQUÍ y
            // también es deliberado: las dos formas son UTF-8 válido, así que
            // el códec las deja literales y esta fixture llevaría dos cadenas
            // que se pintan IGUAL. El fallo de una fixture así sería invisible
            // en la revisión. Va en `types.rs`, con los bytes como escapes.
            "overwrite_dest_spelt_differently",
            SyncStep {
                dest_rel: Some(rel_path("notas/informe%FF%FE.dat")),
                size: Some(31),
                ..sync_step(
                    11,
                    Kind::Overwrite,
                    "NOTAS/informe%FF%FE.dat",
                    Crit::Size,
                    Conf::Certain,
                    Some(Rev::RestoreTrash),
                )
            },
        ),
    ]
}

/// Los pasos que BORRAN, que son de `Mirror` y llevan la misma pareja de
/// reversas: con papelera se saca de ella, sin papelera no se saca de ningún
/// sitio y el plan lo dice antes de que nadie apruebe.
fn sync_step_cases_deleting() -> Vec<(&'static str, norte_proto::methods::SyncStep)> {
    use norte_proto::methods::{
        CompareConfidence as Conf, CompareCriterion as Crit, StepReversal as Rev,
        SyncReason as Why, SyncStep, SyncStepKind as Kind,
    };
    vec![
        (
            "delete_tree",
            sync_step(
                6,
                Kind::DeleteTree,
                "rancio",
                Crit::Presence,
                Conf::Certain,
                Some(Rev::RestoreTrash),
            ),
        ),
        (
            "delete_tree_irreversible",
            SyncStep {
                reason: Some(Why::NoTrashOnTarget),
                ..sync_step(
                    7,
                    Kind::DeleteTree,
                    "rancio",
                    Crit::Presence,
                    Conf::Certain,
                    Some(Rev::Irreversible),
                )
            },
        ),
    ]
}

/// Los pasos que NO tocan nada: uno por motivo, y ninguno con reversa.
fn sync_step_cases_skipped() -> Vec<(&'static str, norte_proto::methods::SyncStep)> {
    use norte_proto::methods::{
        CompareConfidence as Conf, CompareCriterion as Crit, SyncReason as Why, SyncStep,
        SyncStepKind as Kind,
    };
    vec![
        (
            "skip_ambiguous_source",
            SyncStep {
                reason: Some(Why::AmbiguousSource),
                ..sync_step(
                    8,
                    Kind::Skip,
                    "LEEME%FF.txt",
                    Crit::Presence,
                    Conf::Certain,
                    None,
                )
            },
        ),
        (
            // Sin `size`: un `Skip` no mueve bytes, y `counts.bytes` es la suma
            // de ese campo — un tamaño aquí sería un byte contado que nadie
            // escribió, en el número con el que se aprueba el plan.
            "skip_unknown_confidence",
            SyncStep {
                reason: Some(Why::UnknownConfidence),
                ..sync_step(
                    9,
                    Kind::Skip,
                    "empaquetado/dentro.txt",
                    Crit::Mtime,
                    Conf::Unknown,
                    None,
                )
            },
        ),
        (
            "skip_unreadable",
            SyncStep {
                reason: Some(Why::Unreadable),
                ..sync_step(
                    10,
                    Kind::Skip,
                    "secreto",
                    Crit::Presence,
                    Conf::Unknown,
                    None,
                )
            },
        ),
        (
            // 0.43.0 (#207): la pareja del KELVIN. LAS DOS ORTOGRAFÍAS viajan
            // —`rel` con U+212A y `dest_rel` con la `K` ASCII— porque son el
            // punto de la fila: quien la lea tiene que poder ver que los dos
            // nombres NO son el mismo texto. Sin `size`, como todo `Skip`.
            "skip_non_injective_pairing",
            SyncStep {
                reason: Some(Why::NonInjectivePairing),
                dest_rel: Some(norte_proto::methods::RelPath::parse_wire("K.txt").expect("rel")),
                ..sync_step(
                    12,
                    Kind::Skip,
                    "\u{212A}.txt",
                    Crit::Size,
                    Conf::Certain,
                    None,
                )
            },
        ),
    ]
}

/// El BLOQUEO (0.40.0, ADR 0049): las cinco clases, y el `side` presente
/// exactamente cuando el bloqueo es de un lado. `dest_read_only` es del árbol
/// entero, así que su `rel` es la RAÍZ — la forma que un frontend tiene que
/// saber pintar sin nombre que enseñar.
///
/// `type_mismatch_dir` va DOS veces porque su `side` es lo que lo hace legible:
/// el mismo bloqueo con `left` y con `right` son dos frases distintas («no copio
/// un árbol del origen sobre un fichero» / «no borro un árbol del destino para
/// poner un fichero»), y congelar una sola dejaría la otra sin fixture.
#[test]
fn golden_sync_blocker() {
    use norte_proto::methods::{RelPath, Side, SyncBlocker, SyncBlockerKind as Kind};
    check_family(
        "sync_blocker.json",
        &[
            (
                "ambiguous_dest",
                SyncBlocker {
                    rel: rel_path("LEEME%FF.txt"),
                    kind: Kind::AmbiguousDest,
                    side: Some(Side::Right),
                },
            ),
            (
                "dest_read_only",
                SyncBlocker {
                    rel: RelPath::default(),
                    kind: Kind::DestReadOnly,
                    side: Some(Side::Right),
                },
            ),
            (
                "dir_too_large",
                SyncBlocker {
                    rel: rel_path("fotos"),
                    kind: Kind::DirTooLarge,
                    side: Some(Side::Right),
                },
            ),
            (
                // 0.52.0 (#163): un nombre que el destino no puede tener. El
                // lado es SIEMPRE el destino — es su sistema de ficheros el
                // que lo rehúsa, no el origen el que lo escribió mal.
                "illegal_dest_name",
                SyncBlocker {
                    rel: rel_path("CON"),
                    kind: Kind::IllegalDestName,
                    side: Some(Side::Right),
                },
            ),
            (
                // El solape es de las DOS raíces a la vez: no hay un lado que
                // nombrar, y `side` se omite en vez de inventar uno.
                "overlap_detected",
                SyncBlocker {
                    rel: rel_path("sub"),
                    kind: Kind::OverlapDetected,
                    side: None,
                },
            ),
            (
                // Un directorio del ORIGEN contra un fichero del destino, con
                // un nombre no-UTF8 para que el `rel` del bloqueo pase por el
                // mismo códec que el de un paso.
                "type_mismatch_dir_source",
                SyncBlocker {
                    rel: rel_path("informe%FF.d"),
                    kind: Kind::TypeMismatchDir,
                    side: Some(Side::Left),
                },
            ),
            (
                // Y al revés: el árbol que se borraría está en el DESTINO.
                "type_mismatch_dir_dest",
                SyncBlocker {
                    rel: rel_path("build"),
                    kind: Kind::TypeMismatchDir,
                    side: Some(Side::Right),
                },
            ),
        ],
    );
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

/// Familia `host.*` (0.37.0/0.38.0, #131): enumeración de volúmenes del
/// host. `Volume::mount` es un [`VPath`] — `volume_hostile_no_sizes` usa un
/// mount point NO-UTF8 para demostrar el round-trip byte a byte (regla dura
/// 1), y la MISMA fixture pinea la forma de "sin sizes": `total_bytes`/
/// `free_bytes` ausentes del wire, jamás un cero disfrazado de "desconocido"
/// (diseño §A de `2026-08-10-volumes-design.md`).
///
/// V3.5 (0.38.0): `Volume::label` es `Option<Vec<u8>>`, base64 en el wire —
/// `volume` pinea el caso normal (`"USB Nico"` codificado), y la MISMA
/// fixture `volume_hostile_no_sizes` que ya llevaba el mount no-UTF8 gana
/// TAMBIÉN un label no-UTF8 (`\xFF\xFE`), así que un solo fixture demuestra
/// que ninguno de los dos campos-bytes del tipo pasa por un `String`.
fn check_methods_host(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{HostVolumesParams, HostVolumesResult, Volume, VolumeKind};

    check_one(
        fixtures,
        "host_volumes_params",
        &HostVolumesParams {
            include_pseudo: false,
        },
    );
    check_one(
        fixtures,
        "host_volumes_params_pseudo",
        &HostVolumesParams {
            include_pseudo: true,
        },
    );

    let removable = Volume {
        mount: vpath("file:///media/USB-Nico"),
        label: Some(b"USB Nico".to_vec()),
        fs_type: "vfat".into(),
        kind: VolumeKind::Removable,
        total_bytes: Some(64_000_000_000),
        free_bytes: Some(12_000_000_000),
        read_only: false,
    };
    check_one(fixtures, "volume", &removable);

    // Non-UTF8 mount point Y label + la forma "sin sizes" — ver el
    // comentario de la función.
    let hostile_no_sizes = Volume {
        mount: vpath("file:///media/informe%FF%FE"),
        label: Some(vec![0xFF, 0xFE]),
        fs_type: "nfs4".into(),
        kind: VolumeKind::Network,
        total_bytes: None,
        free_bytes: None,
        read_only: true,
    };
    check_one(fixtures, "volume_hostile_no_sizes", &hostile_no_sizes);

    check_one(
        fixtures,
        "host_volumes_result",
        &HostVolumesResult {
            volumes: vec![removable, hostile_no_sizes],
        },
    );

    // Decode-only (asimétrico, como `plugin_help_result_absent`): un `kind`
    // que este cliente no conoce degrada a `Unknown` por `#[serde(other)]`
    // en vez de tirar toda la respuesta de `host.volumes` — el core JAMÁS
    // emite este valor, así que no hay dirección de encode que pinear.
    let future_kind: Volume = serde_json::from_value(
        fixtures
            .get("volume_future_kind")
            .expect("[methods.json] falta la fixture volume_future_kind")
            .clone(),
    )
    .expect("[methods/volume_future_kind] deserialize");
    assert_eq!(
        future_kind.kind,
        VolumeKind::Unknown,
        "un kind desconocido degrada a Unknown, no rompe la decodificación"
    );

    // Decode-only, sin fixture registrada (protocol-guardian MINOR, V3.5
    // review): `label` con base64 ilegible degrada ESE CAMPO a `None`, no
    // el `Volume` entero — el contrato que el rustdoc de `label_wire`
    // promete. `mount`/`fs_type` siguen intactos, que es justo lo que
    // demuestra que el resto de la entrada no se perdió con el campo malo.
    let bad_label_json = serde_json::json!({
        "mount": "file:///media/usb",
        "label": "esto no es base64 !!",
        "fs_type": "vfat",
        "kind": "removable",
        "read_only": false
    });
    let bad_label: Volume =
        serde_json::from_value(bad_label_json).expect("[methods/label malo] deserialize");
    assert_eq!(bad_label.label, None, "base64 ilegible degrada a None");
    assert_eq!(bad_label.mount, vpath("file:///media/usb"));
    assert_eq!(bad_label.fs_type, "vfat");

    // Mismo contrato para un payload DECODIFICABLE pero sobre el tope
    // (`ATTR_BYTES_MAX`, reutilizado — ver el rustdoc de `label_wire`).
    let oversized = norte_proto::attrs::ATTR_BYTES_MAX + 1;
    let oversized_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        vec![0u8; oversized],
    );
    let oversized_json = serde_json::json!({
        "mount": "file:///media/usb",
        "label": oversized_b64,
        "fs_type": "vfat",
        "kind": "removable",
        "read_only": false
    });
    let oversized_label: Volume =
        serde_json::from_value(oversized_json).expect("[methods/label gordo] deserialize");
    assert_eq!(
        oversized_label.label, None,
        "un label decodificado por encima del tope también degrada a None"
    );
}
