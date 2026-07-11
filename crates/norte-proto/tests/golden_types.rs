//! Golden tests de los tipos del protocolo (spec §12): cada fixture JSON es
//! el wire format congelado. Match estructural exacto (`serde_json::Value`)
//! bidireccional — el orden de claves y el formato de whitespace NO son parte
//! del contrato JSON-RPC; nombres, tipos y valores sí. Romper uno de estos
//! tests = cambio de wire format = bump de versión + revisión doble.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::path::Path;

use norte_proto::methods::{
    FsCopyParams, FsDeleteParams, FsListParams, FsListResult, FsMoveParams, FsStatParams,
    FsStatResult, FsTaskResult, TaskCancelParams, TaskCancelResult,
};
use norte_proto::{
    ByteRange, Capabilities, CapabilityFlags, CollisionPolicy, ConflictKind, Entry, EntryKind,
    Error, SymlinkPolicy, TaskId, TaskKind, TaskProgress, TaskState, VPath,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn vpath(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido de fixture")
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
                    path: vpath("file:///home/user/doc.txt"),
                    kind: EntryKind::File,
                    size: Some(1234),
                    mtime_ms: Some(1_720_000_000_000),
                },
            ),
            (
                "dir_no_meta",
                Entry {
                    path: vpath("file:///home/user"),
                    kind: EntryKind::Dir,
                    size: None,
                    mtime_ms: None,
                },
            ),
            (
                "symlink",
                Entry {
                    path: vpath("file:///ln"),
                    kind: EntryKind::Symlink,
                    size: None,
                    mtime_ms: None,
                },
            ),
            (
                "other_pre_epoch",
                Entry {
                    path: vpath("file:///dev-thing"),
                    kind: EntryKind::Other,
                    size: None,
                    mtime_ms: Some(-86_400_000),
                },
            ),
            (
                "hostile_name",
                Entry {
                    path: vpath("file:///informe%FF%FE.dat"),
                    kind: EntryKind::File,
                    size: Some(0),
                    mtime_ms: None,
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

#[test]
fn golden_methods() {
    let sample_entry = Entry {
        path: vpath("file:///home/user/doc.txt"),
        kind: EntryKind::File,
        size: Some(1234),
        mtime_ms: Some(1_720_000_000_000),
    };
    let fixtures = load("methods.json");

    check_one(
        &fixtures,
        "fs_list_params",
        &FsListParams {
            path: vpath("file:///home/user"),
        },
    );
    check_one(
        &fixtures,
        "fs_list_result",
        &FsListResult {
            entries: vec![sample_entry.clone()],
        },
    );
    check_one(
        &fixtures,
        "fs_stat_params",
        &FsStatParams {
            path: vpath("file:///home/user/doc.txt"),
        },
    );
    check_one(
        &fixtures,
        "fs_stat_result",
        &FsStatResult {
            entry: sample_entry,
        },
    );
    check_one(
        &fixtures,
        "fs_copy_params",
        &FsCopyParams {
            from: vpath("file:///src/a.txt"),
            to: vpath("file:///dst/a.txt"),
            on_collision: CollisionPolicy::Fail,
            symlinks: SymlinkPolicy::Preserve,
        },
    );
    check_one(
        &fixtures,
        "fs_copy_params_policies",
        &FsCopyParams {
            from: vpath("file:///src/a.txt"),
            to: vpath("file:///dst/a.txt"),
            on_collision: CollisionPolicy::RenameAuto,
            symlinks: SymlinkPolicy::Skip,
        },
    );
    check_one(
        &fixtures,
        "fs_move_params",
        &FsMoveParams {
            from: vpath("file:///src/dir"),
            to: vpath("sftp://nas:22/backup/dir"),
            on_collision: CollisionPolicy::Fail,
            symlinks: SymlinkPolicy::Preserve,
        },
    );
    check_one(
        &fixtures,
        "fs_delete_params",
        &FsDeleteParams {
            path: vpath("file:///tmp/victim"),
            mode: norte_proto::DeleteMode::Trash,
        },
    );
    check_one(
        &fixtures,
        "fs_delete_params_permanent",
        &FsDeleteParams {
            path: vpath("file:///tmp/victim"),
            mode: norte_proto::DeleteMode::Permanent,
        },
    );
    check_one(
        &fixtures,
        "fs_task_result",
        &FsTaskResult {
            task_id: TaskId::new(7),
        },
    );
    check_one(
        &fixtures,
        "task_cancel_params",
        &TaskCancelParams {
            task_id: TaskId::new(7),
        },
    );
    check_one(&fixtures, "task_cancel_result", &TaskCancelResult {});
    assert_eq!(fixtures.len(), 12, "[methods.json] fixtures sin caso Rust");
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
    assert_eq!(norte_proto::PROTOCOL_VERSION, "0.3.0");
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
}
