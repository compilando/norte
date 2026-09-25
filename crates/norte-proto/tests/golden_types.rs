//! Golden tests for the protocol types (spec §12): each JSON fixture is
//! the frozen wire format. Exact structural match (`serde_json::Value`)
//! bidirectional — key order and whitespace formatting are NOT part of
//! the JSON-RPC contract; names, types, and values are. Breaking one of
//! these tests = a wire format change = version bump + double review.

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
    VPath::parse(wire).expect("valid fixture wire")
}

/// A [`PlanHash`] from its hex form (0.36.0). Fixtures use SYNTHETIC
/// hashes: a real sha256 of something would let a hasher that feeds
/// nothing through. The type validates all the same, which is the point.
fn plan_hash(hex: &str) -> norte_proto::methods::PlanHash {
    norte_proto::methods::PlanHash::parse(hex).expect("fixture plan hash")
}

/// A BASE name from its raw bytes (0.36.0): the rename batch fixtures are
/// written in bytes, not in percent-encoded form — which is exactly what
/// the golden has to demonstrate.
/// A path RELATIVE to a plan's roots (0.40.0) from its wire form.
fn rel_path(wire: &str) -> norte_proto::methods::RelPath {
    norte_proto::methods::RelPath::parse_wire(wire).expect("fixture rel")
}

fn seg(b: &[u8]) -> norte_proto::Segment {
    norte_proto::Segment::new(b.to_vec()).expect("segment")
}

fn load(name: &str) -> BTreeMap<String, Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/types")
        .join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    serde_json::from_str(&raw).expect("valid fixture JSON")
}

/// Checks a whole family: 1:1 coverage between fixture and Rust cases,
/// and an exact match in both directions for each case.
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
        "[{file}] the Rust cases and the fixtures must cover each other 1:1"
    );

    for (name, value) in cases {
        let expected = &fixtures[*name];
        let serialized = serde_json::to_value(value).expect("serializable");
        assert_eq!(&serialized, expected, "[{file}/{name}] serialize");
        let back: T = serde_json::from_value(expected.clone())
            .unwrap_or_else(|e| panic!("[{file}/{name}] deserialize: {e}"));
        assert_eq!(&back, value, "[{file}/{name}] deserialize == constructed");
    }
}

/// Checks a single case against its fixture entry (heterogeneous families).
fn check_one<T>(fixtures: &BTreeMap<String, Value>, name: &str, value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let expected = fixtures
        .get(name)
        .unwrap_or_else(|| panic!("[methods.json] missing fixture {name}"));
    assert_eq!(
        &serde_json::to_value(value).expect("serializable"),
        expected,
        "[methods/{name}] serialize"
    );
    let back: T = serde_json::from_value(expected.clone())
        .unwrap_or_else(|e| panic!("[methods/{name}] deserialize: {e}"));
    assert_eq!(&back, value, "[methods/{name}] deserialize == constructed");
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
                // An id that LOOKS hostile (very long, with underscores) but
                // is LEGAL: exactly `ATTR_ID_MAX` bytes, so it survives
                // decoding's filter and the bidirectional match stays exact.
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

// The FROZEN table of the entire taxonomy. Splitting it up by length
// would hide exactly what `check_family` checks —1:1 coverage between
// fixture and variant—, so here the length is the point.
#[expect(
    clippy::too_many_lines,
    reason = "one assertion per fixture and variant: the length is the point"
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
            // The UI session's cap (0.48.0, L2). Frozen like the other two:
            // the token is the ONLY thing that distinguishes "trims the
            // history" from "there are too many entries", and it is a string.
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
            // 0.84.0 (ADR 0151). Its OWN fixture, not shared with
            // `escapes_root`: a closed vocabulary carries one per value
            // precisely so renaming one does not go unnoticed, and these
            // two are similar enough for someone to lump them together.
            (
                "conflict_destination_gone",
                Error::Conflict {
                    conflict: ConflictKind::DestinationGone,
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
            // The THREE from #279's closed vocabulary: they go one by one
            // because what this golden freezes is the vocabulary, and a
            // single fixture would let the other two rename themselves
            // without anything noticing.
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
            // 0.63.0 (#325): the connection asks for a secret that is
            // nowhere to be found. Its own fixture because it is one more
            // category in the "this cannot continue without a human"
            // family, and with only one from the family the others could
            // rename themselves without anything noticing.
            (
                "secret_needed",
                // The `endpoint` goes in the fixture because it is what
                // makes the dialog answerable, and without it nothing would
                // stop someone from removing it "because the name is already
                // there" (#325).
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
            // 0.36.0 (batch rename): the executor's two negatives. Both mean
            // "nothing was attempted", and both are actionable from the
            // frontend (re-plan).
            ("plan_stale", Error::PlanStale),
            ("plan_not_executable", Error::PlanNotExecutable),
            // 0.40.0 (sync): the two roots are the same tree. ALL THREE
            // relations, because `relation` is the only thing the variant
            // says and because "they are the same" is NOT a degenerate case
            // of "one is inside the other": it is the sentence the frontend
            // shows.
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
            // 0.41.0 (#178): this session's journal cannot be opened and the
            // mutation is refused. No fields, and that is half the contract:
            // the file's path and `SQLite`'s error are local to the process
            // that emits it and do not cross the boundary.
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

// One fixture per `TaskKind` the core emits, with each one's progress
// SEMANTICS written next to it. It is a table: splitting it up by length
// would hide that coverage is one per class.
#[expect(
    clippy::too_many_lines,
    reason = "one assertion per progress class: the length is the coverage"
)]
#[test]
fn golden_task_progress() {
    check_family(
        "task_progress.json",
        &[
            (
                // 0.53.0 (#251): an `fs.dir_size` that finished having left
                // subtrees unread. It is the ONLY case that freezes the
                // `unreadable` name and its shape: the others leave it at
                // `None` and so do not emit it, so without this one
                // renaming or nesting it would not turn anything red.
                //
                // And `Some(0)` is not `None`: "I counted them and there
                // were none" is one answer, "I am not counting them" is
                // another, and confusing them is what makes a client paint
                // a short total with a confident face.
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
                // 0.33.0 (M4-IA-2): TaskKind::Embed on the wire.
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
                // 0.36.0 (batch rename): TaskKind::RenameBatch on the wire,
                // and with it the SEMANTICS of a batch's progress —
                // `entries_*` counts plan STEPS (2 of 3), and `bytes_*` is
                // `None` because a rename moves no bytes. A frontend that
                // painted a byte bar here would paint zero forever.
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
                // 0.60.0 (#314): TaskKind::SetMode. Its progress counts
                // ENTRIES and NOT bytes —a `chmod` moves none—, and the
                // total is known from the start because they are the paths
                // that were sent. Freezing that shape is what stops someone
                // from painting a byte bar that would stay at zero.
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
                // 0.59.0 (#311): TaskKind::Checksum, and the SHAPE of its
                // progress, which the rustdoc promises and until now froze
                // nothing: there is an entry total from the start —how many
                // paths were requested is known— and `bytes_total` is
                // `None`, because how much they weigh is not known without
                // having read them. `unreadable` counts the ones left
                // without a digest (#251).
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
                // 0.49.0 (#139): TaskKind::DirSize, and the fixture that
                // did not get written when the method landed (a
                // `protocol-guardian` finding). Its progress is the ONLY
                // one whose `bytes_done` IS the result — there is no result
                // type—, and that is why the totals go to `None` until the
                // terminal snapshot: a bar toward a made-up number would be
                // worse than none at all.
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
                // 0.50.0 (#132): TaskKind::Pack. `bytes_*` counts what was
                // READ from the source, not what was written: how much the
                // archive will weigh is decided by the compressor, and
                // promising that total would be promising a number that is
                // going to be wrong. `entries_*` does have a total, because
                // the entries are enumerated before starting.
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
                // 0.50.0 (#132): TaskKind::TestArchive. The entries are
                // known (they are in the index) but not how many bytes need
                // reading until they have been read — a zip declares sizes
                // that the test exists precisely not to trust.
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
                // 0.50.0 (#132): TaskKind::Split. Both totals are known
                // from the start —the file's size and the whole split—, so
                // it is one of the few honest bars end to end. `current` is
                // the CHUNK being written.
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
                // 0.50.0 (#132): TaskKind::Combine, the reverse: the chunks
                // are enumerated and measured BEFORE writing anything —which
                // is what lets it reject a gap without having created the
                // destination—, so it also carries both totals.
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
                // 0.39.0 (ADR 0048): TaskKind::Compare on the wire, and
                // with it the SEMANTICS of a comparison's progress —
                // `entries_*` counts PAIRS emitted, and `bytes_*` is
                // zero/`None` because with the hash rung off not a single
                // byte is read. The total is `None` on purpose: the walk
                // does not know how many pairs there are until it finishes
                // walking both trees.
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
                // 0.40.0 (ADR 0049): TaskKind::SyncPlan on the wire, and
                // with it the SEMANTICS of its progress — `entries_*`
                // counts STEPS emitted and `bytes_*` is zero/`None`, exactly
                // as in `Compare`: planning writes not a byte, and with the
                // hash rung off it does not read any either.
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
                // 0.40.0 (ADR 0049): TaskKind::Sync, the OTHER half and the
                // one that does move bytes. It is the contrast that makes
                // the one above legible: same family, `bytes_*` populated,
                // because here copying does happen. Both tokens reach a
                // 0.39 client WITHOUT it having called anything —
                // `task.progress` is broadcast to every human connection—,
                // so freezing their spelling is freezing this bump's only
                // N/N-1 surface.
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
                // 0.31.0 (#104): TaskKind::Mkdir on the wire.
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

/// The STANDALONE types of the rename batch (0.36.0): the requested pair,
/// the plan step, and the verdict. All of them carry BASE names like
/// `Segment`, so each fixture also demonstrates that a non-UTF-8 name
/// travels percent-encoded and comes back byte for byte (hard rule 1).
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
            // HOSTILE name on both sides: the wire escapes it, the
            // round-trip returns the exact bytes.
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
            // The TWO machinery steps: going into the temp name and COMING
            // OUT of it. `temp` belongs to the STEP, not to `to` — the one
            // that takes the file out carries it in `from` and is machinery
            // just the same, so both get pinned.
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
    // The THREE classes of the closed vocabulary, one fixture each. That
    // they all stay THAT is not guaranteed by this `check_family` — it
    // compares fixtures against this hand-written list, so a fourth variant
    // with neither of the two things goes unnoticed —, but by the
    // cross-check against the artifact in `schema.rs`
    // (`the_rename_collision_kind_schema_covers_the_golden_s_verdicts`),
    // which IS generated from the type.
    //
    // Each with a DISTINCT `pair_index`: it is the field that does not
    // depend on `kind` and the one that lets the guilty row be pointed at
    // under a verdict the client does not understand.
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
            // The source is IN EXCESS instead of missing: the requested name
            // folds onto two directory entries and matches neither exactly.
            // `name` is the source exactly as whoever asked for it wrote it.
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

/// The plan is wire-frozen: a step, a collision, and the hash have fixed
/// field names, and the names are percent-encoded segments.
#[test]
fn golden_fs_rename_batch_plan_result() {
    use norte_proto::methods::{
        FsRenameBatchPlanResult, RenameCollision, RenameCollisionKind, RenameStep,
    };
    check_family(
        "fs_rename_batch_plan_result.json",
        &[
            // THE REFERENCE SHAPE: the `a→b, b→a` permutation, the case
            // that is impossible today with N loose `fs.move`s. There are
            // THREE steps, and the third is the one that closes it: without
            // `.norte-rename-… → b`, the file that started as `a` stays
            // parked under the machine name and `b` never comes to exist.
            // The planner is written against this fixture, so a truncated
            // plan here would be a truncated planner there.
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
            // The DEAD plan, and its shape matters as much as the one
            // above: `steps` EMPTY. A plan with verdicts is not partially
            // ordered — the planner emits no steps for it —, so a caller
            // that ignored `executable` would have nothing to execute
            // anyway. Steps and collisions do NOT coexist in anything the
            // core emits (design decision 4), and a fixture that mixed them
            // would pin a shape that does not exist.
            //
            // Two verdicts on different pairs: the index is what makes the
            // row addressable, and the hostile name travels percent-encoded.
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
            // The executable plan: an empty `collisions` is an EMPTY LIST
            // on the wire, never an absent key nor `null`. The hash is
            // SYNTHETIC on purpose: a real sha256 of something — the empty
            // input's, for example — would let this golden pass a hasher
            // that feeds nothing through.
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
    check_methods_journal(&fixtures);
    check_methods_organize(&fixtures);
    // 98 → 101 in 0.32.0: + ai_rename_plan_params/result/result_empty (M4-IA,
    // ADR 0031). 101 → 106 in 0.33.0: + index_embed_params,
    // index_search_semantic_params(/_no_root)/result and semantic_hit
    // (M4-IA-2, ADR 0031 A3). 106 → 113 in 0.34.0: + plugin_info_with_help,
    // plugin_help_params and
    // plugin_help_result(/_flags/_empty/_lossy/_absent) (H3e, ADR 0040 — the
    // last three are the empty page, the hostile one, and the ABSENT field).
    // 113 → 114 in 0.35.0: + plugin_column_values_params_scoped (#120 — the
    // request that NAMES the plugin; the one that does not name it keeps its
    // fixture byte for byte, which is what `skip_serializing_if` promises).
    // 114 → 116 in 0.36.0: + fs_rename_batch_plan_params and fs_rename_batch_params
    // (the rename batch; the plan's RESULT has its own file, because its
    // family pins several plan shapes). 116 → 120: + fs_rename_batch_report_params and
    // fs_rename_batch_report_result(/_clean/_uncertain) — the batch's report:
    // clean, stuck, and with the unknown destination step.
    // 120 → 126 in 0.37.0 (#131): + host_volumes_params(/_pseudo),
    // host_volumes_result and the standalone types volume/volume_hostile_no_sizes/
    // volume_future_kind (the "without sizes" shape and the `serde(other)` degrade).
    // 126 → 130 in 0.39.0 (ADR 0048): + fs_compare_params(/_minimo) and
    // compare_rows_batch(/_empty) — the request with everything populated and
    // the MINIMAL one (which is the one that freezes the defaults), plus the
    // batch and its empty shape. The ROW has its own file (`compare_row.json`):
    // its family pins one shape per verdict.
    // 130 → 142 in 0.40.0 (ADR 0049): + sync_plan_params(/_minimo),
    // sync_steps_batch(/_empty), sync_plan_done(/_blocked/_opaque),
    // sync_apply_params, sync_report_params and
    // sync_report_result(/_clean/_died). The STEP and the BLOCKER have their
    // own file (`sync_step.json`, `sync_blocker.json`): their families pin
    // one shape per class. The plan's THREE closures are the three trash
    // kinds ([`DestTrash`]), which is what decides whether the plan can be undone.
    // 142 → 144 in 0.46.0 (roadmap item 10): + daemon_shutdown_params_handover
    // and daemon_going_away. The handover has its OWN fixture instead of
    // changing the stop's, which is what lets you see at a glance that an
    // ordinary stop's message has not changed a byte.
    // 144 → 148 in 0.48.0 (L2): + session_get_result, session_get_result_empty,
    // session_put_params and session_put_result. GET and PUT carry the SAME
    // session on purpose: what the fixture demonstrates is that the body
    // comes back the same as it went. The EMPTY one has its own fixture
    // because it is the one that comes out on every first boot and the only
    // one with `body: null`.
    // 148 → 151 in 0.49.0: + fs_dir_size_params (#139) and the two for
    // `connection.close` (#140). The RESULT has no
    // fixture of its own because it has no type of its own — it is the
    // usual `FsTaskResult`, already frozen.
    // 151 → 156 in 0.50.0 (#132): + archive_pack_params, archive_test_params,
    // archive_test_result, file_split_params and file_combine_params. The
    // three results of pack/split/combine have no fixture because they have
    // no type of their own — they are the usual `FsTaskResult`, already
    // frozen. Unpacking does not appear at all: it is an `fs.copy`, and its
    // shape has been frozen since 0.10.
    // 156 → 158 when applying the review: + archive_test_report_params (the
    // FIFTH method of the bump, which was not frozen anywhere) and
    // archive_test_result_clean (the shape that a truly healthy archive
    // returns: with every field `serde(default)`, a clean result is `{}`
    // on the wire, and no golden was pinning it).
    // 158 → 161 in 0.53.0: + plugin_info_with_digest and
    // plugin_set_approval_params_anchored (#282, the anchor a human read
    // traveling both ways) and plugin_list_result_dir_bytes (#265, the
    // basename's bytes). All three fields are optional, so without these
    // fixtures their NAME and their wire shape —a sha256's hex, `label_wire`'s
    // base64— were frozen by nothing.
    // 161 → 164 in 0.54.0 (#295): + fs_list_result_anchored,
    // fs_copy_params_anchored and fs_move_params_anchored. All three fields
    // are optional and are omitted, so without these fixtures neither the
    // field's NAME nor its wire shape —an opaque hex string, never an
    // inode— were frozen by anything.
    // 0.57.0 (#290): fs_create_params with its percent-encoded name, and its
    // anchored counterpart — `fs.create` carries `dest_anchor` and
    // `fs.mkdir` does not, which is what needs freezing.
    // 166 → 169 in 0.58.0 (#250): + archive_pack_report_params and the
    // report's two shapes. The `fold` (`unicode`/`case`/`full`) and `risk`
    // (`separator`/`stream`/`reserved`/`trailing`) tokens are wire
    // vocabulary and these fixtures are the only thing freezing them — one
    // per value, because a single one would let the others be renamed
    // without anything noticing. And the CLEAN shape goes separately
    // because it means something on its own: "twelve entries were checked
    // and there was nothing", which is not the same as a daemon that does
    // not check.
    // 169 → 172 in 0.59.0 (#311): + fs_checksum_params and the report's two
    // shapes. The `miss` tokens (`unreadable`/`not_a_file`) are wire
    // vocabulary and this fixture is the only thing freezing them — both in
    // the same one, because they go in the same list. And a path that is
    // NOT UTF-8 among them: the report has to be able to name the file that
    // could not be read even when its name is not text (rule 1).
    // 172 → 173 in 0.60.0 (#314): + fs_set_mode_params, with the mode in its
    // NUMERIC shape and a path that is not UTF-8.
    // 173 → 175 in 0.62.0 (#315, #121): + fs_set_mode_params_recursivo and
    // ai_rename_plan_params_seleccion. Both are SEPARATE fixtures and not one
    // more field on the existing ones, because the three new fields are
    // OMITTED when empty: with a single fixture per method, the day they
    // stopped being omitted —or `recursive`'s default changed— the wire
    // would change without anything turning red.
    // 178 → 186 in 0.64.0 (#322): + `connection.failed`, with ONE fixture
    // PER VALUE of its closed vocabulary (seven) plus the one that carries
    // NEITHER of the two optional fields. A single one would let the other
    // six be renamed without anything turning red, and `reason` is compared
    // by equality in the frontend: a silent rename is a sentence that stops
    // showing up.
    // 186 → 196 in 0.65.0 (#328): + the ten for `log.tail`/`log.level`. Five
    // of them are ONE PER VALUE of the level vocabulary: with a single
    // fixture the other four could be renamed without anything turning red,
    // and `level` is compared by equality —to color a row, to mark which
    // one is set, and to decide what gets captured—, so a silent rename is
    // a panel that stops coloring. The other five freeze the two shapes
    // that mean something on their own: the request WITHOUT a cursor (which
    // travels as explicit `null` and means "whatever you have", not "from
    // the start") and the poll that found nothing (`lines: []` with
    // `lost: 0`, which is the most frequent answer and the only one that
    // distinguishes "nothing has happened" from "something was lost").
    // 196 → 198 in 0.66.0 (D4): + `span_wire_bg` and
    // `plugin_preview_styled_params_columns`. SEPARATE fixtures because both
    // fields are omitted when missing: the earlier ones prove the old wire
    // did not move, these prove the new one exists.
    // 198 → 200 in 0.67.0 (ADR 0095): + `plugin_command_info_renamer` and
    // `plugin_rename_plan_params`.
    // 200 → 201 in 0.68.0 (#332): + `ai_rename_plan_result_refused`.
    // 201 → 203 in 0.69.0 (ADR 0100): + `plugin_notice_notify` and
    // `plugin_notice_hooks_disabled`, ONE PER VALUE of the `kind` vocabulary:
    // the frontend decides by equality whether to translate the class or
    // paint the text.
    // 203 → 205 in 0.70.0 (ADR 0101): + `plugin_notice_effect_denied` and
    // `plugin_info_with_hook_badges` — the `hook:<event>` and
    // `fs-write:<name>` badges a hook shows on approval; without a fixture,
    // nothing froze its wire shape.
    // 205 → 207 in 0.71.0 (ADR 0104): + `plugin_uninstall_params` and
    // `plugin_uninstall_result`.
    // 207 → 209 in 0.72.0 (ADR 0105): + `plugin_decorate_params_with_kinds`
    // and `plugin_decorate_result_icon` — the class's and the slot's wire
    // names, which nothing froze without a fixture.
    // 209 → 214 in 0.74.0 (phase 3): the five for `plugin.panel_render`.
    // Three are ONE PER VARIANT of `PanelEvent`: the enum is tagged AND
    // flattened over the params, so its wire shape —`{"event": "click",
    // "row": 2, …}`— is frozen by nothing but this, and renaming a case or
    // moving a field would turn nothing red. The fourth carries real lines
    // and zones (a span is a `SpanWire`, the same as a styled preview's, not
    // a twin with the color in another encoding). And the fifth is the
    // ABSENT frame: the field travels with `flatten`, so "no panel" is `{}`
    // and not `null` — the empty fixture is the only thing that leaves it written.
    // 214 → 217 in 0.75.0 (phase 4): the three for `fs.dir_usage`. The
    // report is HALFWAY on purpose —`pending > 0` and `partial` at `true`—,
    // which is what a cancelled Task over a tree with an unreadable folder
    // leaves written: the two signals that distinguish "the map is
    // complete" from "the map is what could be read", and that nobody
    // freezes without a fixture. And a child's name carries a byte that is
    // not UTF-8, because a disk map shows real names and that is the shape
    // the wire has to preserve.
    // 217 → 223 in 0.76.0 (phase 7): the six for the timeline. `params`
    // appears twice because an absent `before_seq` serializes as explicit
    // `null` and means "from the newest", which is a different question
    // than a number; and `result` appears twice because the END of the list
    // is `next_before_seq` at `null`, and a shape that only exists when
    // paging finishes is frozen by nobody if it is not written. The row
    // carries batch, compensation, and a sanitized path with its `hostile`
    // flag: they are the three fields an undo confirmation's truthfulness
    // depends on.
    // 223 → 229 in 0.77.0 (phase 8): the six for organize. The
    // `proposed_rel` with subdirectories is the shape that really needs
    // freezing —it is the only thing separating this plan from the rename
    // one— and the result appears TWICE, with a plan and with `refused`,
    // because the receiver's rule ("with a reason, the plan does not
    // count") is frozen by nobody if only one is written.
    // 231 → 232 in 0.80.0: `journal.undo_after` with a ceiling. The one
    // without a ceiling stays: it is the one that demonstrates 0.79's wire
    // did not change.
    assert_eq!(
        fixtures.len(),
        232,
        "[methods.json] fixtures with no Rust case"
    );
}

/// `log.tail` and `log.level` (0.65.0, #328): the DAEMON's log.
///
/// What these fixtures freeze is the closed vocabulary of levels —five
/// strings compared by equality on both ends of the wire—, that an absent
/// cursor travels as an explicit `null` and not as a zero, and that an empty
/// poll is `lines: []` with `lost: 0`. The three things are the difference
/// between a panel that tells the truth about what happened and one with a
/// silent gap.
fn check_methods_log(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{LOG_LEVELS, LogLevelParams, LogLevelResult, LogTailParams};
    // One fixture per vocabulary value. The loop goes over `LOG_LEVELS` so
    // adding a level without its fixture turns red here, instead of going
    // unnoticed until a frontend cannot color it.
    for level in LOG_LEVELS {
        check_one(
            fixtures,
            &format!("log_level_params_{level}"),
            &LogLevelParams {
                level: (*level).to_owned(),
            },
        );
    }
    // The result is not a `bool`: the ring NEVER lowers its level, so asking
    // for `warn` while the ring is already at `debug` answers `debug`. That
    // is not a failure, and with a `bool` you would have to lie with a
    // `true` or alarm with a `false`.
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
    check_methods_log_rest(fixtures);
}

/// Organize (0.77.0, phase 8): the plan, its refusal, and the batch that applies it.
///
/// The `proposed_rel` with subdirectories is the shape that really needs
/// freezing: it is the only thing that distinguishes this plan from the
/// rename one, and it is the string the core uses to decide whether to
/// create folders.
fn check_methods_organize(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        AiOrganizePlanParams, AiOrganizePlanResult, FsOrganizeParams, OrganizeMove,
        PluginOrganizePlanParams,
    };
    check_one(
        fixtures,
        "ai_organize_plan_params",
        &AiOrganizePlanParams {
            dir: vpath("file:///casa/descargas"),
            instruction: "ordénalas por año".to_owned(),
            names: vec!["factura.pdf".to_owned()],
        },
    );
    check_one(
        fixtures,
        "organize_move",
        &OrganizeMove {
            current: "factura.pdf".to_owned(),
            proposed_rel: "facturas/2026/marzo.pdf".to_owned(),
        },
    );
    check_one(
        fixtures,
        "ai_organize_plan_result",
        &AiOrganizePlanResult {
            moves: vec![OrganizeMove {
                current: "factura.pdf".to_owned(),
                proposed_rel: "facturas/2026/marzo.pdf".to_owned(),
            }],
            refused: None,
            // The token travels WITH the plan, and this fixture freezes it:
            // without it, a reviewed plan could not be redeemed for anything.
            plan_hash: Some(
                norte_proto::methods::PlanHash::parse(&"ab".repeat(32)).expect("64 hex"),
            ),
        },
    );
    // A refusal: `refused` set and `moves` empty. The RECEIVER's rule is
    // that with a reason the plan does not count, and this fixture is the
    // one that leaves it written in bytes.
    check_one(
        fixtures,
        "ai_organize_plan_result_refused",
        &AiOrganizePlanResult {
            moves: Vec::new(),
            refused: Some("aprueba mi capacidad `location`".to_owned()),
            // Without a plan there is no token, and the field is omitted
            // entirely: a refusal produces the SAME bytes as before the
            // token existed.
            plan_hash: None,
        },
    );
    check_one(
        fixtures,
        "plugin_organize_plan_params",
        &PluginOrganizePlanParams {
            plugin_id: "org.acme.orden".to_owned(),
            organizer_id: "por-año".to_owned(),
            dir: vpath("file:///casa/descargas"),
            names: vec!["factura.pdf".to_owned()],
        },
    );
    check_one(
        fixtures,
        "fs_organize_params",
        &FsOrganizeParams {
            dir: vpath("file:///casa/descargas"),
            moves: vec![OrganizeMove {
                current: "factura.pdf".to_owned(),
                proposed_rel: "facturas/2026/marzo.pdf".to_owned(),
            }],
            plan_hash: norte_proto::methods::PlanHash::parse(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .expect("test hash"),
        },
    );
}

/// The journal's timeline (0.76.0, phase 7): the params' two shapes, the
/// result's two, the row with everything that distinguishes it, and the
/// undo's cutoff.
fn check_methods_journal(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        JournalListParams, JournalListResult, JournalRow, JournalUndoAfterParams,
    };
    // An absent `before_seq` serializes as explicit `null` and means "from
    // the newest", which is what a screen requests on opening: a separate
    // case for the same reason as `log_tail_params_sin_cursor`.
    check_one(
        fixtures,
        "journal_list_params",
        &JournalListParams {
            before_seq: Some(4_096),
            limit: 50,
            actor_kind: Some("user".to_owned()),
        },
    );
    check_one(
        fixtures,
        "journal_list_params_desde_el_final",
        &JournalListParams {
            before_seq: None,
            limit: 50,
            actor_kind: None,
        },
    );
    // A row with EVERYTHING that distinguishes it: a batch, a compensation,
    // and a path that had to be sanitized —with its flag—. The `path`
    // already comes masked by the server and `hostile` is what stops it
    // from being read as faithful.
    check_one(
        fixtures,
        "journal_row",
        &JournalRow {
            seq: 4_096,
            ts_ms: 1_756_000_000_000,
            actor_kind: "user".to_owned(),
            actor_id: None,
            op: "renamed".to_owned(),
            path: "file:///a/%EF%BF%BDgpj.exe".to_owned(),
            path_to: Some("file:///a/antes".to_owned()),
            hostile: true,
            reversible: true,
            undoes_seq: None,
            undone: false,
            batch_id: Some(7),
        },
    );
    // The cursor SET and the cursor at `null` are the two shapes of the
    // list's end, and the second is the one that says "there is nothing
    // older left". Freezing only one would leave the end with no written shape.
    check_one(
        fixtures,
        "journal_list_result",
        &JournalListResult {
            rows: vec![JournalRow {
                seq: 12,
                ts_ms: 1_756_000_000_000,
                actor_kind: "agent".to_owned(),
                actor_id: Some("s-1".to_owned()),
                op: "created".to_owned(),
                path: "file:///a/x".to_owned(),
                path_to: None,
                hostile: false,
                reversible: true,
                undoes_seq: Some(11),
                undone: false,
                batch_id: None,
            }],
            next_before_seq: Some(12),
        },
    );
    check_one(
        fixtures,
        "journal_list_result_final",
        &JournalListResult {
            rows: Vec::new(),
            next_before_seq: None,
        },
    );
    check_one(
        fixtures,
        "journal_undo_after_params",
        // Without a ceiling: 0.79's wire, which does not change.
        &JournalUndoAfterParams {
            seq: 4_096,
            upto_seq: None,
        },
    );
    check_one(
        fixtures,
        "journal_undo_after_params_con_techo",
        &JournalUndoAfterParams {
            seq: 4_096,
            upto_seq: Some(4_200),
        },
    );
}

/// The rest of the log fixtures: `log.tail` and its empty case. Separated
/// from [`check_methods_log`] only for length.
fn check_methods_log_rest(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{LogLine, LogTailParams, LogTailResult};
    // An absent `cursor` serializes as explicit `null` (ADR 0004; an
    // `Option` with no `skip`), and that shape is what a panel requests on
    // opening. It is a separate case on purpose: `null` means "whatever you
    // have" and `0` means "from the first line that ever existed", which
    // against a ring that already wrapped around would force answering a
    // huge and false `lost`.
    check_one(
        fixtures,
        "log_tail_params_sin_cursor",
        &LogTailParams {
            cursor: None,
            max: 500,
        },
    );
    // The second line is from `suppaftp` and is at INFO: the ring's cap lets
    // third-party lines through up to there and not one level more, no
    // matter what `log.level` says. And `target` travels WHOLE, which is
    // what makes that cap possible and also the reader's per-subsystem filter.
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
    // The poll that found nothing, which is the most frequent answer: an
    // EMPTY list and `lost: 0`. The fixture exists because `lines` is not
    // omitted — the day someone put `skip_serializing_if` on it, "nothing
    // has happened" and "the field did not come" would stop being
    // distinguishable on the wire.
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

/// `fs.dir_size` (0.49.0, #139): what gets frozen is that paths travel as a
/// LIST —a selection is measured all at once— and in `VPath`'s wire shape,
/// not as loose text.
fn check_methods_dir_size(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{ConnectionCloseParams, ConnectionCloseResult, FsDirSizeParams};
    // `connection.close` (#140): what gets frozen is that it closes by a
    // PATH —the frontend does not need to know how a session is keyed— and
    // that the result says whether there was anything to close.
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

/// Archive-writing family (0.50.0, #132).
///
/// What gets frozen: the FORMAT travels as a closed, explicit token —it is
/// not deduced from the name on the server, see `ARCHIVE_PACK`—, the BASE
/// always travels because without it the saved names are undefined, and the
/// test's result says WHAT it checked as well as what failed: "passes"
/// means different things in a zip and in a plain tar, and without
/// `checked` a client would paint "intact" over a format that has nothing
/// to back that up.
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
                // The WHOLE path in wire shape: it is what points out WHICH
                // of the two `x.txt` in an archive is corrupt, and the only
                // one that preserves the bytes of a name that is not UTF-8.
                path: "zip+file:///a.zip/!/roto.txt".to_owned(),
                name: "roto.txt".to_owned(),
                reason: "crc".to_owned(),
            }],
            truncated: false,
            checked: vec!["crc".to_owned()],
        },
    );
    // A HEALTHY archive, which is the ordinary answer: no failures and
    // saying what it checked. The three `checked` tokens are wire
    // vocabulary and this golden is the only thing freezing them.
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
    check_methods_fs_dir_usage(fixtures);
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

/// `fs.set_mode` (0.60.0, #314): a batch's POSIX permissions.
///
/// With a path that is NOT UTF-8, because changing the permissions of a
/// file whose name is not text has to be requestable all the same (rule 1),
/// and with the mode in its numeric shape: `0o755` travels as 493, and
/// freezing that here is what stops someone from turning it into
/// `"rwxr-xr-x"` without realizing that is a wire change.
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
    // And the RECURSIVE shape (0.62.0, #315), which is a different request:
    // both fields present at once, because `dir_mode` without `recursive`
    // means nothing. A single fixture would let `recursive`'s default
    // change without anything turning red.
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
    // And the THIRD shape, the one that breaks trees: recursive with the
    // SAME mode for everything (`dir_mode` absent). It is a request
    // different from the other two, and the one `chmod -R` does, so its
    // wire is frozen separately.
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

/// `fs.checksum` and its report (0.59.0, #311): checking that a file is the
/// one someone published.
///
/// The report's fixture carries BOTH `miss` reasons —the only thing
/// freezing those wire tokens— and a path that is NOT UTF-8: the report has
/// to be able to name the file that could not be read even when its name is
/// not text.
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
            // HALFWAY ON PURPOSE: `pending > 0` is what a cancelled Task's
            // report leaves written, and freezing it here is what stops
            // someone from zeroing it "for tidiness".
            pending: 2,
        },
    );
}

/// `fs.dir_usage` (0.75.0, phase 4): what a directory is made of.
///
/// The report is frozen HALFWAY —`pending` different from zero and
/// `partial` at `true`— because that is the state that really matters: a
/// cancelled Task's, or a tree's with a folder that would not let itself be
/// read. A map that said neither of the two things reads as complete.
fn check_methods_fs_dir_usage(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        DirUsageChild, FsDirUsageParams, FsDirUsageReportParams, FsDirUsageReportResult,
    };
    use norte_proto::{EntryKind, Segment};

    let seg = |b: &[u8]| Segment::new(b.to_vec()).expect("segment");
    check_one(
        fixtures,
        "fs_dir_usage_params",
        &FsDirUsageParams {
            path: norte_proto::VPath::parse("file:///casa").expect("wire"),
            depth: 1,
        },
    );
    check_one(
        fixtures,
        "fs_dir_usage_report_params",
        &FsDirUsageReportParams {
            task_id: norte_proto::TaskId::new(9),
        },
    );
    check_one(
        fixtures,
        "fs_dir_usage_report_result",
        &FsDirUsageReportResult {
            children: vec![
                DirUsageChild {
                    name: seg(b"docs"),
                    kind: EntryKind::Dir,
                    bytes: 4096,
                    entries: 12,
                    partial: false,
                },
                // A name that is NOT UTF-8: a disk map shows real names, and
                // the wire has to preserve them as they are.
                DirUsageChild {
                    name: seg(b"caf\xFF.txt"),
                    kind: EntryKind::File,
                    bytes: 17,
                    entries: 1,
                    // A lower bound, and PER CHILD: it is the rectangle the
                    // map has to mark, and what a global flag cannot say.
                    partial: true,
                },
            ],
            total_bytes: 4113,
            total_entries: 13,
            pending: 2,
            listed: true,
            // There are more children than fit, and their bytes ARE in the
            // totals: what is lost is their name, not their size.
            omitted: 3,
        },
    );
}

/// `archive.pack_report` (0.58.0, #250): what that packaging saved and that
/// means something else outside.
///
/// The four `risk` tokens are wire vocabulary and these goldens are the
/// only thing freezing them — one per value, because a single fixture would
/// let the other three be renamed without anything noticing.
///
/// What has NO fixture are fold collisions, and it is deliberate: those are
/// not packaged —`archive.pack` fails with `Exists` before writing a byte—
/// so the report has nowhere to carry them.
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
                // A name that is NOT UTF-8: `path` is the only thing the
                // bytes can be recovered from —`name` carries the `U+FFFD`
                // from painting it—, and without this fixture the property
                // that codec exists for was frozen by nothing (rule 1).
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
    // And the CLEAN report, which is the ordinary answer and the one that
    // means something on its own: twelve entries were checked and there was
    // nothing.
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

/// UI `session.*` family (0.48.0, L2): the screen the daemon saves. The
/// golden freezes that `body` travels AS IS —an arbitrary object, neither
/// wrapped nor re-serialized to a string— and that `owner` goes in GET's
/// result and not inside the session: who is in charge belongs to the
/// CHANNEL, not the document.
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
    // The EMPTY session, which is the most common answer in this whole
    // wire: every installation's first `session.get`. `body` is `null` and
    // NOT `{}` — the only case where it is not an object—, so if something
    // changed it to `{}` no other golden would notice.
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
    // 0.78.0 (phase 9): the TWO answers of `session.release`, because
    // `false` is not an error but a fact —"it was not you"— and whoever
    // hands off decides with it whether to launch the other frontend. A
    // single fixture would leave exactly the half that reads badly unfrozen.
    check_one(
        fixtures,
        "session_release_result",
        &norte_proto::methods::SessionReleaseResult { released: true },
    );
    check_one(
        fixtures,
        "session_release_result_not_owner",
        &norte_proto::methods::SessionReleaseResult { released: false },
    );
}

/// `fs.rename_batch*` family (0.36.0): the plan and execution REQUESTS. The
/// intention (`pairs`) is the only thing the client sends — the core
/// decides the order —, and execution adds the `plan_hash` the human approved.
fn check_methods_rename_batch(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        FsRenameBatchParams, FsRenameBatchPlanParams, FsRenameBatchReportParams,
        FsRenameBatchReportResult, RenamePair, RenameStuckStep,
    };
    // HOSTILE dir + an `a→b, b→a` permutation: the case that motivates the method.
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
    // The EXECUTION request points to the EXECUTABLE plan from
    // `fs_rename_batch_plan_result.json`: same pairs and its same hash.
    // With the dead plan's — `"0"×64` — this fixture would be a
    // legal-looking request for a plan the core has to reject, and anyone
    // who copied the fixture into a task 7 test would write that test backwards.
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
    // The report that MOTIVATES the method: the rollback got stuck, so
    // there is a file under a name nobody asked for and the report NAMES
    // it. With the hostile name, which is where a `String` would have lied.
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
    // The shape NO other fixture covers: the step whose destination is
    // unknown (`uncertain`), with no journal entry behind it (`journalled:
    // false` — nobody but a human is going to undo it) and with lost
    // compensations. It is the worst possible outcome and exactly the one
    // the method exists for: if its shape is not frozen, the one that
    // matters is not either.
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
    // A clean run: absent things are OMITTED, and `compensations_lost`
    // travels at zero like the rest of the counters.
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

/// `ai.*` family (0.32.0, M4-IA, ADR 0031): a reviewable rename plan.
fn check_methods_ai(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::VPath;
    use norte_proto::methods::{AiRenameEntry, AiRenamePlanParams, AiRenamePlanResult};
    // HOSTILE dir (non-UTF-8, percent-encoded in VPath's wire).
    check_one(
        fixtures,
        "ai_rename_plan_params",
        &AiRenamePlanParams {
            dir: VPath::parse("file:///home/user/fotos-a%FF%FE").unwrap(),
            instruction: "kebab-case, date first".into(),
            names: Vec::new(),
        },
    );
    // The plan over a SELECTION (0.62.0, #121): the names travel and the
    // directory stays the same. A separate fixture because the field is
    // OMITTED when empty — with a single one, the day it stops being
    // omitted nobody notices.
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
    // An empty plan = the model proposed no changes (a meaningful state, not
    // an omitted case): pins the wire shape, not just the happy path.
    check_one(
        fixtures,
        "ai_rename_plan_result_empty",
        &AiRenamePlanResult {
            entries: vec![],
            refused: None,
        },
    );
    // 0.68.0 (#332): an empty plan WITH a reason — the plugin refused and
    // said why. A separate fixture: the two above prove 0.67's wire did not
    // move; this one, that the field exists.
    check_one(
        fixtures,
        "ai_rename_plan_result_refused",
        &AiRenamePlanResult {
            entries: vec![],
            refused: Some("this renamer needs the `location` capability".into()),
        },
    );
}

/// `index.*` family (0.25.0, M4, ADR 0034): build + query of the search index.
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
    // A hit with a HOSTILE name (non-UTF-8, percent-encoded in VPath's wire).
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
    // (direct, cancellable). Scores EXACT in binary (0.5) so f64's
    // round-trip has nothing to round.
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
    // Pins `root`'s ABSENCE on the wire (default + skip_serializing_if): no
    // key, not `"root": null`.
    check_one(
        fixtures,
        "index_search_semantic_params_no_root",
        &IndexSearchSemanticParams {
            root: None,
            query: "informe".into(),
            k: 20,
        },
    );
    // A hit with a HOSTILE name (non-UTF-8, percent-encoded in VPath's wire).
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

/// RPC LAYER family (0.19.0, #72): `rpc.cancel`.
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

/// plugin.* family (0.13.0, M4-P3): catalogue + human approval/enabling.
fn check_methods_plugin(fixtures: &BTreeMap<String, Value>) {
    check_methods_plugin_governance(fixtures);
    check_methods_plugin_exec(fixtures);
    check_methods_plugin_data_out_v2(fixtures);
    check_methods_plugin_config(fixtures);
    check_methods_plugin_help(fixtures);
}

/// `plugin.help` cases (H3e, 0.34.0): [`PluginInfo`]'s `has_help` and the
/// method's two types. Its own function because of
/// `check_methods_plugin_info`'s line limit, whose goldens do NOT change —
/// that they stay byte for byte the same is exactly what demonstrates the
/// field is strongly additive.
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
            panels: Vec::new(),
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
    // The "no page" shape the contract PROMISES: an empty string and both
    // flags low, never an error (see `markdown`'s rustdoc).
    check_one(
        fixtures,
        "plugin_help_result_empty",
        &PluginHelpResult {
            markdown: String::new(),
            truncated: false,
            lossy: false,
        },
    );
    // HOSTILE, and MIXED flags (bytes can be lost without hitting the cap):
    // the `U+FFFD` that `lossy` describes travels VERBATIM, and with it a
    // `U+202E` bidi override that flips the text that follows it
    // ("gnp.exe" reads as "exe.png"). Masking is the FRONTEND's job when
    // rendering — the wire transports, it does not sanitize —, so the
    // fixture keeps the danger on purpose: if someone ever "cleans up" the
    // text in proto, this golden is what turns red.
    check_one(
        fixtures,
        "plugin_help_result_lossy",
        &PluginHelpResult {
            markdown: "Ver\u{FFFD}sion \u{202E}gnp.exe".to_owned(),
            truncated: false,
            lossy: true,
        },
    );
    // An ABSENT `markdown` reads as the empty page — normative since the
    // field's rustdoc, and until now without a fixture. It does NOT go
    // through `check_one`: it is deliberately asymmetric (on emission the
    // field is never omitted), so only the direction the contract
    // promises, the INPUT one, is checked.
    let absent: PluginHelpResult = serde_json::from_value(
        fixtures
            .get("plugin_help_result_absent")
            .expect("[methods.json] missing fixture plugin_help_result_absent")
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
        "a peer that omits `markdown` is saying \"there is no page\""
    );
}

/// `plugin.notice` (0.69.0, ADR 0100): ONE fixture PER `kind` VALUE. With
/// only one, renaming the other would turn nothing red, and the frontend
/// compares `kind` by equality to decide whether to translate the class or
/// paint `text`.
fn check_methods_plugin_notice(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{PLUGIN_NOTICE_KINDS, PluginNotice};
    assert_eq!(
        PLUGIN_NOTICE_KINDS,
        &["notify", "hooks-disabled", "effect-denied"]
    );
    check_one(
        fixtures,
        "plugin_notice_notify",
        &PluginNotice {
            plugin_id: "org.norte.rename-log".into(),
            kind: "notify".into(),
            text: Some("renamed 3 files".into()),
        },
    );
    // Without `text`: proof that it does not travel when there is none.
    check_one(
        fixtures,
        "plugin_notice_hooks_disabled",
        &PluginNotice {
            plugin_id: "org.norte.rename-log".into(),
            kind: "hooks-disabled".into(),
            text: None,
        },
    );
    check_one(
        fixtures,
        "plugin_notice_effect_denied",
        &PluginNotice {
            plugin_id: "org.norte.rename-log".into(),
            kind: "effect-denied".into(),
            text: None,
        },
    );
}

/// [`PluginInfo`]/[`PluginCommandInfo`] cases (P1, 0.26.0): the shape
/// without `description`/`commands`, the shape WITH both populated, and the
/// standalone `PluginCommandInfo` type. Its own function so as not to
/// overflow `check_methods_plugin_governance`'s line limit.
// A LITERAL list of golden cases: each one is a frozen wire shape with its
// reason, and splitting it into arbitrary halves would only hide which ones there are.
#[expect(
    clippy::too_many_lines,
    reason = "each fixture carries its reason; splitting it in half would hide which ones there are"
)]
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
    // 0.67.0 (ADR 0095): a renamer among the commands, with its `kind`. The
    // fixture above proves an ordinary command still does not carry it.
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
            panels: vec![],
            has_help: false,
            manifest_digest: None,
        },
    );
    // ADR 0100/0101: what a hook shows on approval are its events and the
    // files it can write, in the OPEN list of badges.
    check_one(
        fixtures,
        "plugin_info_with_hook_badges",
        &PluginInfo {
            id: "org.norte.rename-log".into(),
            name: "Rename log".into(),
            publisher: "norte".into(),
            version: "0.1.0".into(),
            category: "hook".into(),
            capabilities: vec![
                "location".into(),
                "hook:after-renamed".into(),
                "fs-write:.norte-renames.log".into(),
            ],
            approved: true,
            enabled: true,
            description: None,
            commands: vec![],
            columns: vec![],
            panels: vec![],
            has_help: false,
            manifest_digest: None,
        },
    );
    // (P1/G3c) description + commands + columns POPULATED: a new golden,
    // does not replace the one above (which still covers the shape without them).
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
            panels: vec![],
            has_help: false,
            manifest_digest: None,
        },
    );
    // 0.53.0 (#282): the anchor that travels with the catalogue and comes back with the yes.
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
            panels: vec![],
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
                panels: vec![],
                has_help: false,
                manifest_digest: None,
            }],
            errors: vec![PluginLoadError {
                // The BASENAME, never the absolute path: that would reveal
                // the user's home to an agent calling `plugin.list`, and
                // the field's rustdoc declares it an invariant. The
                // previous golden froze `/plugins/broken`, i.e. the opposite.
                dir: "broken".into(),
                reason: "manifiesto inválido".into(),
                dir_bytes: None,
            }],
        },
    );
    // 0.53.0 (#265): with the basename's bytes alongside. It is the case
    // that FREEZES `label_wire`'s base64 shape for this field — without it,
    // nothing pins the alphabet and the padding, and the other case's
    // absence does not even pin the `dir_bytes` NAME.
    check_one(
        fixtures,
        "plugin_list_result_dir_bytes",
        &PluginListResult {
            plugins: vec![],
            errors: vec![PluginLoadError {
                dir: "caf\u{FFFD}".into(),
                reason: "manifiesto inválido".into(),
                // `caf\xff`: the bytes the string above can no longer say,
                // which is the field's reason for existing.
                dir_bytes: Some(vec![b'c', b'a', b'f', 0xFF]),
            }],
        },
    );
}

/// `plugin.panel_render` family (0.74.0, phase 3): the frame a plugin
/// paints into a layout slot.
///
/// The three `params` are ONE PER VARIANT of [`PanelEvent`]: it is a tagged
/// enum, and its wire shape —`{"event": "click", "row": …}`— is pinned by
/// nothing but this. The empty `result` goes separately because the field
/// travels with `#[serde(flatten)]`: "no frame" is `{}` and not `null`, and
/// whoever reads it by comparing against `null` would never see an absent panel.
fn check_methods_plugin_panel(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        PanelEvent, PanelFrame, PanelHit, PluginPanelRenderParams, PluginPanelRenderResult,
        SpanWire,
    };

    let base = |event: PanelEvent| PluginPanelRenderParams {
        plugin_id: "org.norte.git-panel".into(),
        kind: "git".into(),
        dir: vpath("file:///home/user/proyecto"),
        cols: 40,
        rows: 8,
        lang: "es".into(),
        cursor_name: Some("README.md".into()),
        state: None,
        event,
    };
    check_one(
        fixtures,
        "plugin_panel_render_params",
        &base(PanelEvent::Refresh),
    );
    check_one(
        fixtures,
        "plugin_panel_render_params_click",
        &PluginPanelRenderParams {
            // With state: they are opaque bytes from the guest, and on the
            // wire they travel in base64 with a ceiling on deserialization.
            state: Some(b"rama=main".to_vec()),
            ..base(PanelEvent::Click { row: 2, col: 5 })
        },
    );
    check_one(
        fixtures,
        "plugin_panel_render_params_command",
        &base(PanelEvent::Command {
            command: "nav.enter".into(),
        }),
    );
    check_one(
        fixtures,
        "plugin_panel_render_result",
        &PluginPanelRenderResult {
            frame: Some(PanelFrame {
                plugin_id: "org.norte.git-panel".into(),
                // The span is a `SpanWire`, the SAME as a styled preview's:
                // a color is three bytes, not a hex string.
                lines: vec![vec![SpanWire {
                    text: "main".into(),
                    role: Some("title".into()),
                    fg: None,
                    bg: None,
                }]],
                hits: vec![PanelHit {
                    row: 0,
                    col: 0,
                    width: 4,
                    command: "nav.enter".into(),
                    arg: Some("file:///home/user/proyecto/.git".into()),
                }],
                state: Some(b"rama=main".to_vec()),
            }),
        },
    );
    check_one(
        fixtures,
        "plugin_panel_render_result_none",
        &PluginPanelRenderResult { frame: None },
    );
}

/// GOVERNANCE `plugin.*` family (0.13.0, M4-P3): listing and approving/enabling.
fn check_methods_plugin_governance(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        PluginListParams, PluginSetApprovalParams, PluginSetApprovalResult, PluginSetEnabledParams,
        PluginSetEnabledResult, PluginUninstallParams, PluginUninstallResult,
    };
    // `plugin.list` with no params: empty golden, symmetry with `task_list_params`.
    check_one(fixtures, "plugin_list_params", &PluginListParams {});
    check_methods_plugin_info(fixtures);
    check_methods_plugin_notice(fixtures);
    check_methods_plugin_panel(fixtures);
    check_one(
        fixtures,
        "plugin_set_approval_params",
        &PluginSetApprovalParams {
            id: "org.norte.demo".into(),
            approved: true,
            expected_digest: None,
        },
    );
    // 0.53.0 (#282): with the anchor the human read. Freezes the field's
    // name and its shape (a sha256's lowercase hex), which is what the
    // daemon compares byte for byte before granting.
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
    // 0.71.0 (ADR 0104): uninstalling over the wire. The result says
    // whether there was consent, which is what just stopped existing.
    check_one(
        fixtures,
        "plugin_uninstall_params",
        &PluginUninstallParams {
            id: "org.norte.demo".into(),
        },
    );
    check_one(
        fixtures,
        "plugin_uninstall_result",
        &PluginUninstallResult { was_approved: true },
    );
}

/// EXECUTION `plugin.*` family (0.14.0/0.15.0, M4-P4/P5): running and previewing.
fn check_methods_plugin_exec(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        PluginPreview, PluginPreviewParams, PluginPreviewResult, PluginRunCommandParams,
        PluginRunCommandResult,
    };
    // `plugin.run_command` (0.14.0, M4-P4): `arg` without skip → always on the wire.
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
    // `plugin.preview` (0.15.0, M4-P5): a populated result (flatten at the
    // root) and the empty one (`None` → `{}`); a partial one is
    // inexpressible (test in types.rs).
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
    // 0.29.0 (#101): the lossy decoding warning populated (`true`).
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

/// CONFIGURATION `plugin.*` family (0.28.0, G3c, ADR 0037): schema +
/// effective value (`plugin.get_config`) and persisting a value
/// (`plugin.set_config`). `plugin_config_key_wire_bare` covers the minimal
/// shape (min/max/description absent, values empty — all
/// `skip_serializing_if`/additive always present as appropriate);
/// `plugin_config_key_wire_full` the shape with EVERYTHING populated (`int`
/// type with min/max, which are the struct's only optional fields).
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

/// STRUCTURED data v2 `plugin.*` family (0.27.0, G3, ADR 0037): the host
/// paints, never the plugin. Covers a styled preview (same all-or-nothing
/// `flatten`-over-`Option` pattern as [`PluginPreviewResult`]), 1:1
/// POSITIONAL decorations, and 1:1 POSITIONAL columns. Split into two
/// functions (styled preview / decorate+columns) for clippy's line limit,
/// same criterion as `check_methods_plugin_info`.
fn check_methods_plugin_data_out_v2(fixtures: &BTreeMap<String, Value>) {
    check_methods_plugin_preview_styled(fixtures);
    check_methods_plugin_decorate_and_columns(fixtures);
}

/// 0.66.0 (D4): a span's background, and the viewer's width in the
/// request. SEPARATE fixtures from 0.27.0's: both fields are omitted when
/// missing, so the earlier ones prove the old wire did not move and these
/// that the new one exists.
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

/// `plugin.preview_styled` (0.27.0): same all-or-nothing pattern as
/// `plugin.preview`, with `lines: Vec<Vec<SpanWire>>` instead of `output: String`.
fn check_methods_plugin_preview_styled(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::PluginPreviewStyledResult;
    use norte_proto::methods::{PluginPreviewStyled, PluginPreviewStyledParams, SpanWire};
    // A standalone span: the minimal shape (only `text`, role/fg omitted by
    // `skip_serializing_if`) and the POPULATED shape (role+fg together on
    // the wire, even though the host paints with `role` when both are present).
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
    // 0.29.0 (#101): parity with `plugin_preview_result_lossy` — the styled
    // variant also pins `lossy: true` on the wire.
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

/// `plugin.decorate` + `plugin.column_values` (0.27.0): both 1:1
/// POSITIONAL with `params.paths`. `plugin_decorate_params` includes a
/// HOSTILE name (non-UTF-8); `plugin_decorate_result`'s second element is
/// `{}` (no badge/role from THAT plugin for THAT entry), not an omitted element.
#[expect(
    clippy::too_many_lines,
    reason = "one fixture per wire shape: the list is literal on purpose"
)]
fn check_methods_plugin_decorate_and_columns(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        DecorationSlot, DecorationWire, PluginColumnValuesParams, PluginColumnValuesResult,
        PluginDecorateParams, PluginDecorateResult, PluginDecorations,
    };
    check_one(
        fixtures,
        "plugin_decorate_params",
        &PluginDecorateParams {
            paths: vec![
                vpath("file:///repo/a.rs"),
                vpath("file:///repo/informe%FF%FE.dat"),
            ],
            kinds: Vec::new(),
        },
    );
    // 0.72.0 (ADR 0105): with each path's class, positional. Freezes
    // `EntryKind`'s wire names in THIS field.
    check_one(
        fixtures,
        "plugin_decorate_params_with_kinds",
        &PluginDecorateParams {
            paths: vec![vpath("file:///repo/a.rs"), vpath("file:///repo/src")],
            kinds: vec![EntryKind::File, EntryKind::Dir],
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
                slot: DecorationSlot::Badge,
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
    // No decorator answered: `plugins` empty. Unlike preview's `flatten`
    // pattern, here there is no all-or-nothing — an absent plugin is simply
    // an element missing from `plugins`.
    check_one(
        fixtures,
        "plugin_decorate_result_empty",
        &PluginDecorateResult { plugins: vec![] },
    );
    // 0.72.0 (ADR 0105): an ICON decorator says its slot; a badge one does
    // not say it and travels byte for byte as in 0.71 (the fixture above
    // pins it: `slot` absent).
    check_one(
        fixtures,
        "plugin_decorate_result_icon",
        &PluginDecorateResult {
            plugins: vec![PluginDecorations {
                plugin_id: "org.norte.file-icons".into(),
                slot: DecorationSlot::Icon,
                decorations: vec![DecorationWire {
                    badge: Some("📁".into()),
                    role: None,
                }],
            }],
        },
    );
    // `paths` carries TWO entries so `values` can pin both positional
    // cases: a real cell and a `None` (the column does not apply to that
    // entry, distinguishable from a real empty string).
    check_one(
        fixtures,
        "plugin_column_values_params",
        &PluginColumnValuesParams {
            column_id: "git-status".into(),
            paths: vec![vpath("file:///repo/a.rs"), vpath("file:///repo/README")],
            // Without `plugin_id`: it is a 0.34 client's request, and its
            // golden has to keep being THE SAME file as before the bump —
            // that is what `skip_serializing_if` promises.
            plugin_id: None,
        },
    );
    // With `plugin_id` (0.35.0, #120): the new shape, in its own golden.
    check_one(
        fixtures,
        "plugin_column_values_params_scoped",
        &PluginColumnValuesParams {
            column_id: "status".into(),
            paths: vec![vpath("file:///repo/a.rs")],
            plugin_id: Some("org.norte.git".into()),
        },
    );
    // 0.67.0 (ADR 0095): a renamer's plan. The result is the AI's and
    // already has its fixture.
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

/// session.* family (0.12.0, M3-4): an agent session's undo over the wire.
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
    // 0.16.0 (#71): the undo's report over the wire.
    check_one(
        fixtures,
        "policy_undo_report_params",
        &PolicyUndoReportParams {
            task_id: norte_proto::TaskId::new(9),
        },
    );
    // 0.36.0 (batch rename): `batch_stuck` travels ALONGSIDE `blocked` and
    // not instead of it — they say different things ("I stopped, the tree
    // is consistent" versus "I could not return it"), and a fixture that
    // could only carry one of the two would suggest they are exclusive.
    check_one(
        fixtures,
        "policy_undo_report_result",
        &PolicyUndoReportResult {
            undone: 3,
            skipped_irreversible: 1,
            skipped_created_no_trash: 2,
            skipped_not_ours: 1,
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
            // 0.43.0 (#171): the policy denied one unit and the undo
            // CONTINUED. It goes in the same fixture as `blocked` on
            // purpose: the two can come out together and say opposite
            // things —"I stopped" versus "I skipped this one and
            // continued"—, so a fixture that could only carry one would
            // suggest they are exclusive.
            denied: vec![UndoBlocked {
                seq: 37,
                error: norte_proto::Error::PolicyDenied {
                    rule: "scope-expired".into(),
                },
            }],
            denied_total: 1,
        },
    );
    // No blocking: `blocked` and `batch_stuck` are OMITTED
    // (skip_serializing_if), not `null`. `compensations_lost` does travel
    // at zero, like the other counters: an absent counter and a
    // zero-valued counter must not be confusable.
    check_one(
        fixtures,
        "policy_undo_report_result_clean",
        &PolicyUndoReportResult {
            undone: 4,
            skipped_irreversible: 0,
            skipped_created_no_trash: 0,
            skipped_not_ours: 0,
            blocked: None,
            batch_stuck: None,
            compensations_lost: 0,
            // Empty and at zero: like `compensations_lost`, they travel the
            // same way — an absent counter and one at zero must not be confusable.
            denied: Vec::new(),
            denied_total: 0,
        },
    );
}

/// policy.* family (0.11.0, M3-3b): scopes + approvals + `agent_session`.
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
            // 0.36.0: the list is TRIMMED — one path shown out of nine. It
            // is the shape that matters to freeze: with
            // `paths_total == paths.len()` the fixture would demonstrate
            // nothing, and it is exactly the case where a frontend has to
            // warn the human.
            paths_total: 9,
            ttl_ms: 30_000,
            // 0.61.0 (#314): the op that is NOT answered with just the op
            // and the paths. The shape WITH mode is frozen: it is what
            // needs to travel, and the one without it is covered by the two
            // `pending_approval` fixtures, where the field is omitted entirely.
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
            // Without a detail: a `delete` is answered with the op and the
            // paths, and the field is OMITTED from the JSON entirely —
            // which is what makes the bump additive for every other op.
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

/// connection.* family (0.7.0, phase 6): the TOFU flow's `trust_host_key`,
/// and `provide_secret` (0.63.0, #325), which is its twin.
fn check_methods_connection(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::methods::{
        ConnectionDegraded, ConnectionFailed, ConnectionProvideSecretParams,
        ConnectionProvideSecretResult, ConnectionTrustHostKeyParams, ConnectionTrustHostKeyResult,
    };
    // #325. `conn` carries accents and an eñe on purpose: it is a
    // `connections.toml` KEY, i.e. any UTF-8 at all, and this fixture is
    // what stops someone from normalizing it or trimming it on the way to
    // the wire. `secret` is made up: a fixture is not a secret, and without
    // it nothing freezes the params' shape (which is the file's own argument).
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
    // An absent `port` serializes as `null` (Option with no skip): case pinned.
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

    // `connection.failed` (0.64.0, #322): ONE fixture PER VALUE of the
    // closed vocabulary. With only one, renaming any of the other six would
    // turn nothing red — and `reason` is compared by equality in the
    // frontend, so a silent rename is a sentence that stops showing up.
    for (case, reason) in [
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
            case,
            &ConnectionFailed {
                conn: Some("trabajo".into()),
                scheme: "sftp".into(),
                host: "servidor.example".into(),
                reason: reason.into(),
                detail: Some("el secreto de «trabajo» está definido pero VACÍO".into()),
            },
        );
    }
    // And the case WITHOUT the two optionals: proof that they do not travel
    // when they are not there (a typed-in URL has no connection name).
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

/// `fs.list`/`fs.stat` params. The `_con_attrs` cases (0.30.0, ADR 0039) ask
/// for ids; the ones next to them, WITHOUT the field, are the proof of
/// additivity: an empty `attrs` does not travel over the wire.
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

/// fs.* + task.cancel family (list/stat/copy/move/delete/task).
#[expect(
    clippy::too_many_lines,
    reason = "one fixture per fs.* family method, with no logic inside"
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
    // 0.54.0 (#295): the OPAQUE identity of the listed directory, which the
    // client retains so it can say LATER which one it was. Freezes the
    // field's name and its wire shape —a hex string, never an inode nor a
    // volume—, which is the only thing a client can see of it.
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
    // 0.57.0 (#290): fs.create. With a PERCENT-ENCODED name, which is the
    // reason these fixtures exist: what needs freezing is not that the
    // field is called `path`, it is that a name with bytes that are not
    // printable ASCII crosses the wire and comes back UNCHANGED (hard rule 1).
    check_one(
        fixtures,
        "fs_create_params",
        &norte_proto::methods::FsCreateParams {
            path: vpath("file:///tmp/borrador-%FF%FE.txt"),
            dest_anchor: None,
        },
    );
    // And with an anchor: `fs.create` carries it and `fs.mkdir` does not,
    // so the omitted/present pair is needed here just as in copy and move.
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
            name_glob: Some("*.rs".to_owned()),
            name_regex: Some("^ma.n\\.rs$".to_owned()),
            content: Some("año".to_owned()),
            content_regex: Some("a.o".to_owned()),
            case_sensitive: true,
            max_hits: Some(100),
            // The ten filters of 0.81.0, ALL set and none at its absent
            // value: the golden for a field that matches its default does
            // not distinguish "it travels" from "it does not exist".
            // `recursive` at `false` for that same reason, being the only
            // one whose default is `true`.
            kinds: vec![norte_proto::EntryKind::File, norte_proto::EntryKind::Dir],
            min_size: Some(1024),
            max_size: Some(1_048_576),
            mtime_after: Some(1_700_000_000_000),
            mtime_before: Some(1_800_000_000_000),
            exclude_roots: vec![vpath("file:///home/user/.cache")],
            exclude_names: vec!["target".to_owned(), "node_modules".to_owned()],
            whole_word: true,
            recursive: false,
            encoding: Some("windows-1252".to_owned()),
            ..FsSearchParams::new(vpath("file:///home/user"))
        },
    );
    check_one(
        fixtures,
        "fs_search_params_minimo",
        // The minimal one is LITERALLY what `new` builds: no criteria and
        // no filters. Writing it field by field would copy the constructor
        // and let the two copies drift apart.
        &FsSearchParams::new(vpath("file:///home/user")),
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

/// `fs.compare` + `compare.rows` (0.39.0, ADR 0048): the REQUEST and the BATCH.
///
/// `fs_compare_params_minimo` is the one that matters: two roots and
/// nothing else, and even so the wire carries `criteria` WHOLE,
/// `mtime_tolerance_ms`, and `follow_symlinks`. Those defaults decide
/// whether comparing two trees reads content (`hash: false`) and what
/// counts as "the same date" (2000 ms, the FAT rule), so they are frozen
/// explicitly instead of being omitted: a peer that deduced them backwards
/// would read a terabyte nobody asked for.
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
            // Absent in the fixture, and that absence IS 0.39.0's behavior:
            // an orphan, one row. A 0.39 client's minimal request does not
            // change a single byte with the new field (0.40.0).
            descend_orphans: None,
        },
    );
    // Everything populated, and with HOSTILE roots: `max_depth` present (it
    // is omitted when `None`, and this fixture is the one that demonstrates
    // it by contrast), the expensive rung on, and ZERO tolerance — a
    // filesystem that promises nanoseconds on both sides.
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
    // The batch: `task_id` to correlate, and the rows in the order the walk
    // produced them. Never more than `COMPARE_ROWS_MAX_BATCH`.
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
    // An EMPTY batch is an empty list on the wire, never an absent key: the
    // core's pump can close the comparison with no new rows.
    check_one(
        fixtures,
        "compare_rows_batch_empty",
        &CompareRowsBatch {
            task_id: TaskId::new(7),
            rows: vec![],
        },
    );
}

/// `sync.*` family (0.40.0, ADR 0049): the plan's REQUEST, its two
/// notifications, applying it —which carries nothing but the hash— and the report.
///
/// What it freezes, beyond the field names:
///
/// - `sync_plan_params_minimo` is the minimum a client sends —two roots and
///   the mode— with EVERYTHING else at its default, which is what makes
///   this fixture the anchor for those defaults. The mode has no default
///   and so cannot be missing: between copying and deleting there is no
///   neutral value.
/// - `sync_plan_params` carries the two HOSTILE roots, CROSSING PROVIDERS
///   (local → sftp), a populated `include`, the expensive rung on, and
///   `on_unknown: skip`. Neither `descend_orphans` nor `follow_symlinks`
///   appear: they are not the caller's, and sending them is `-32602`.
/// - `sync_plan_done_blocked` freezes the shape —not the number— of the
///   trimmed list: `blockers` is what fits and `blockers_total` is how many
///   there were. And `executable: false` travels even though it could be
///   deduced from the list, for the same reason as in `FsRenameBatchPlanResult`.
/// - `sync_apply_params` has ONE key. It is the method's entire invariant.
/// - `sync_report_result_died` is the only shape where `batch_id` is
///   missing: application died before opening the journal's unit. A report
///   without `batch_id` is a report without undo, so the absent key is a
///   strong claim and has its own fixture.
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

/// The plan's two NOTIFICATIONS: the step batches and the closure.
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
    // An EMPTY batch is an empty list, never an absent key: the pump can
    // close a plan with no new steps to send.
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
                // Not null ON PURPOSE: an N-1 client summing the batches of
                // an N+1 daemon is the only one that fills it, and the
                // golden has to show that the key travels.
                unknown_kind: 2,
                // Same as `unknown_kind`: an `irreversible` alongside a
                // restorable trash is NOT produced by this core —
                // `reversal_for` does not mark as irreversible what the
                // trash can return—, so this fixture is an N+1 daemon's
                // shape, frozen on purpose so a client knows how to read it.
                irreversible: 1,
                bytes: 4096,
                unmeasured_steps: 7,
            },
            blockers: vec![],
            blockers_total: 0,
            executable: true,
            // The trash that DOES return things: it is what makes true the
            // `delete`/`restore_trash` of this same plan's steps.
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
            // And the one that does not exist: the `copy` above says
            // `delete` and still would not come back (undo skips it). The
            // golden freezes the pairing because it is the one a dialog
            // cannot distinguish without this field.
            dest_trash: DestTrash::Absent,
        },
    );
    // The third trash: macOS's and Windows's, which buries without saying
    // where. There NO step is reversible —not even a copy— and that is why
    // `irreversible` equals the sum of the classes that act.
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

/// The second half of the family: applying an approved plan, and its report.
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
        // `failed` counts ALL the failures and `failures` is the trimmed
        // list, so `failures.len() <= failed` always holds. With five rows
        // and a `failed: 4` the golden would show the opposite to whoever
        // reads it to write a client.
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
                // The MOST common hostile row of a `Mirror`, and the one that
                // motivated `SyncFailure::kind` (0.42.0, #195): a denied
                // `DeleteTree`. It carries no `dest_rel` —there is no pair to
                // spell— and its `rel` hangs off the DESTINATION, so before
                // this field the only proof on the wire (`dest_rel` present
                // ⟹ `rel` is the source's) said nothing and the reader had to
                // pick a root blind.
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
                // The fourth class the executor can note, and the one the
                // schema sweep would NOT catch: `SyncStepKind` is pinned
                // against `sync_step.json`, so a class with no FAILURE row
                // here would go unnoticed. `Skip` cannot: it does not
                // execute, so it does not fail (`protocol-guardian`, W4b MINOR-4).
                SyncFailure {
                    rel: rel_path("sub"),
                    dest_rel: None,
                    cause: SyncFailureCause::Denied,
                    kind: SyncStepKind::CreateDir,
                },
                // The legality of the name under the DESTINATION root is not
                // validated when planning, so it surfaces here and with its
                // own name. And with the DESTINATION's spelling, which is the
                // one that failed: the flagship case is a name that blows
                // `NAME_MAX` when recomposed in NFD, and showing bare `rel`
                // would point at the source's short, legal spelling.
                //
                // The pair FROZEN here is the one folded by case, not the
                // NFC/NFD one, since `dest_rel`'s golden in `SyncStep` already
                // warned about that: both Unicode forms are valid UTF-8 and
                // the segment codec leaves them literal, so in a JSON file
                // they render THE SAME — the diff would be unreadable and an
                // editor's normalization would turn the test into a tautology.
                SyncFailure {
                    rel: rel_path("NOTAS/informe.txt"),
                    dest_rel: Some(rel_path("notas/informe.txt")),
                    cause: SyncFailureCause::IllegalName,
                    kind: SyncStepKind::Copy,
                },
            ],
            batch_id: Some(12),
            // A `Mirror` that deletes against a destination with a trash that
            // NAMES what it buries: with this in the report, "can this batch
            // be given back?" is answered without having saved
            // `sync.plan_done` (#170).
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
            // The contrast that makes the field useful: three clean copies,
            // and NONE of this comes back. Without `dest_trash` this report
            // and the one above are the same report to whoever has to decide
            // whether to undo.
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
            // `Opaque`: there is a trash and it does not say where it leaves
            // things. That `batch_id` is `None` does not contradict it —
            // they are two separate questions, and with no batch there is
            // nothing to undo anyway.
            //
            // This fixture is DECODE-only: `new_report` always opens the
            // report with its batch set and is the core's only constructor,
            // so this daemon never produces a report without `batch_id`. It
            // is frozen because the field has been optional on the wire
            // since 0.40.0 and a client has to know how to read the shape
            // without it (`protocol-guardian`, W4b MINOR-6).
            dest_trash: DestTrash::Opaque,
        },
    );
}

/// fs.copy/fs.move (with resume/verify from 0.6.0, ADR 0012).
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
            queued: false,
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
            queued: false,
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
            queued: false,
        },
    );
    // 0.54.0 (#295): the listing's anchor COMING BACK with the request that
    // writes. The two fixtures —copy and move— freeze that the field is
    // named the same in both and that it is OMITTED when absent: without
    // that, a 0.53 client and a 0.54 one with no anchor would not produce
    // the same JSON.
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
            queued: false,
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
            queued: false,
        },
    );
}

/// Daemon methods (ADR 0011): initialize and daemon.shutdown.
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
    // 0.46.0: the handover, and the notification it is announced with.
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

/// 0.5.0 methods (phase 3): task.list, fs.read, fs.capabilities.
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

/// The second half of [`check_methods_v05`]: `fs.capabilities` and its
/// attribute catalogue.
///
/// Split in two because the first one went past a hundred lines when it
/// gained `unvisited` (0.62.0), not because they are two families: they are
/// the same wire version.
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

/// The frozen JSON-RPC envelope (ADR 0011): the shape of request/response/
/// notification and the error object with the taxonomy in `data`.
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
    assert_eq!(
        fixtures.len(),
        7,
        "[envelope.json] fixtures with no Rust case"
    );
}

/// The JSON-RPC CODES and the frame limit are wire-observable: a typo
/// cannot pass CI (protocol-guardian finding m1).
#[test]
fn rpc_codes_and_limits_are_frozen() {
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
// A LIST: one `assert_eq!` per wire name, and it grows with the vocabulary.
// Splitting it into two arbitrary halves would hide half of it, and what
// makes a frozen list useful is seeing it whole — the same criterion as the
// `effect_of` table in the window.
#[expect(
    clippy::too_many_lines,
    reason = "one name per method, frozen all at once"
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
    // policy.* family (0.11.0/0.12.0): agent governance. The pin arrived
    // late (protocol-guardian's MINOR-1 on the 0.12 bump).
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
    // plugin.* family (0.13.0, M4-P3): catalogue + human governance.
    assert_eq!(methods::PLUGIN_LIST, "plugin.list");
    assert_eq!(methods::PLUGIN_SET_APPROVAL, "plugin.set_approval");
    assert_eq!(methods::PLUGIN_SET_ENABLED, "plugin.set_enabled");
    assert_eq!(methods::PLUGIN_RUN_COMMAND, "plugin.run_command");
    assert_eq!(methods::PLUGIN_PREVIEW, "plugin.preview");
    // STRUCTURED-data plugin.* family v2 (0.27.0, G3, ADR 0037): the host
    // paints, never the plugin. Additive over 0.26.x.
    assert_eq!(methods::PLUGIN_PREVIEW_STYLED, "plugin.preview_styled");
    assert_eq!(methods::PLUGIN_DECORATE, "plugin.decorate");
    assert_eq!(methods::PLUGIN_COLUMN_VALUES, "plugin.column_values");
    assert_eq!(methods::PLUGIN_RENAME_PLAN, "plugin.rename_plan");
    assert_eq!(methods::FS_READ_MAX_CHUNK, 8 * 1024 * 1024);
    assert_eq!(methods::FS_LIST_MAX_PAGE, 10_000);
    // 0.34.0 (H3e): the cap on `PluginHelpResult::markdown`. The LITERAL, not
    // the symbol: the contract invites a receiver to size against it, so
    // changing it changes the wire and something has to turn red.
    // `norte-core` separately anchors that this number and the host's are
    // the same one.
    assert_eq!(methods::PLUGIN_HELP_MAX_BYTES, 64 * 1024);
    // 0.18.0 (M4 live search): fs.search + search.hits + TaskKind::Search.
    // Additive over 0.17.x.
    assert_eq!(methods::FS_SEARCH, "fs.search");
    assert_eq!(methods::SEARCH_HITS, "search.hits");
    assert_eq!(methods::SEARCH_HITS_MAX_BATCH, 256);
    // 0.19.0 (#72): rpc.cancel { id }. Additive over 0.18.x.
    assert_eq!(methods::RPC_CANCEL, "rpc.cancel");
    // 0.20.0 (#44): connection.degraded (server→client). Additive over 0.19.x.
    assert_eq!(methods::CONNECTION_DEGRADED, "connection.degraded");
    // 0.21.0 (#55, ADR 0028): tar+gz in ARCHIVE_FORMATS (longest-match). It
    // adds no new method/notification — the bump signals the ability to
    // parse `tar+gz+…` schemes.
    assert!(norte_proto::ARCHIVE_FORMATS.contains(&"tar+gz"));
    // 0.22.0 (#93): optional `skipped` field in FsListResult. It adds no
    // method/notification — the bump signals the listing's additive metadata.
    // 0.23.0 (#95): Error::LimitExceeded{limit} variant — a local limit ≠
    // Corrupt. CLOSED vocabulary pinned here: ONLY the two constants
    // (cd-bytes does NOT exist — max_cd_bytes only gates caching the CD).
    assert_eq!(norte_proto::Error::LIMIT_ENTRIES, "entries");
    assert_eq!(
        norte_proto::Error::LIMIT_DECOMPRESSED_BYTES,
        "decompressed-bytes"
    );
    // 0.24.0 (#56): multi-layer addressing + a nesting cap in the
    // LimitExceeded vocabulary (three constants, still CLOSED).
    assert_eq!(norte_proto::Error::LIMIT_NESTING, "nesting");
    // 0.25.0 (M4, ADR 0034): search index. index.build (Task) + index.query.
    assert_eq!(methods::INDEX_BUILD, "index.build");
    assert_eq!(methods::INDEX_QUERY, "index.query");
    // 0.26.0 (P1): PluginInfo gains description + commands (no new method).
    // 0.27.0 (G3, ADR 0037): plugin.preview_styled/decorate/column_values —
    // structured plugin data, the host paints it.
    assert_eq!(methods::PLUGIN_GET_CONFIG, "plugin.get_config");
    assert_eq!(methods::PLUGIN_SET_CONFIG, "plugin.set_config");
    // 0.28.0 (G3c, ADR 0037): plugin.get_config/set_config — P2's [config]
    // over the wire; PluginInfo gains columns (no new method).
    // 0.29.0 (#101): PluginPreview/PluginPreviewStyled gain `lossy` (no new
    // method — just an additive field).
    // 0.30.0 (columns block 1, ADR 0039): provider attributes — Entry.attrs,
    // FsCapabilitiesResult.attrs and the two request attrs (no new method).
    // 0.31.0 (#104): fs.mkdir (Task) + TaskKind::Mkdir. Additive over 0.30.x.
    assert_eq!(methods::FS_MKDIR, "fs.mkdir");
    assert_eq!(methods::FS_CREATE, "fs.create");
    // 0.32.0 (M4-IA, ADR 0031): ai.rename_plan — a cancellable direct response.
    assert_eq!(methods::AI_RENAME_PLAN, "ai.rename_plan");
    // 0.33.0 (M4-IA-2, ADR 0031 A3): index.embed (Task, TaskKind::Embed) +
    // index.search_semantic (direct, cancellable, k trimmed to the cap).
    assert_eq!(methods::INDEX_EMBED, "index.embed");
    assert_eq!(methods::INDEX_SEARCH_SEMANTIC, "index.search_semantic");
    assert_eq!(methods::INDEX_SEMANTIC_MAX_K, 100);
    // 0.34.0 (H3e): plugin.help — ONE plugin's help page on demand;
    // PluginInfo gains has_help (cheap discovery, no new method).
    assert_eq!(methods::PLUGIN_HELP, "plugin.help");
    // 0.35.0 (#120): PluginColumnValuesParams gains `plugin_id` — no new
    // method, so here only the version moves.
    // 0.36.0 (batch rename): the executor's TWO halves — the reviewable plan
    // (direct response) and its execution (Task, `TaskKind::RenameBatch`),
    // tied together by the `plan_hash` the human approved.
    assert_eq!(methods::FS_RENAME_BATCH_PLAN, "fs.rename_batch_plan");
    assert_eq!(methods::FS_RENAME_BATCH, "fs.rename_batch");
    // …and the batch's report, which is what a `Failed` cannot tell.
    assert_eq!(methods::FS_RENAME_BATCH_REPORT, "fs.rename_batch_report");
    // The LITERALS, not the symbols, for the same reason as
    // `PLUGIN_HELP_MAX_BYTES` above: a receiver sizes against them — it
    // rejects the batch before sending it, it reserves the hash's buffer —
    // so moving them moves the contract and something has to turn red. The
    // pair cap REJECTS (it does not trim like `FS_LIST_MAX_PAGE`), and that
    // is why it matters even more for a third party to know it.
    assert_eq!(methods::FS_RENAME_BATCH_MAX_PAIRS, 4096);
    assert_eq!(methods::PLAN_HASH_LEN, 64);
    // 0.37.0 (#131): host.volumes — enumerating the host's volumes, ONLY for
    // a User connection (design §C of `2026-08-10-volumes-design.md`).
    assert_eq!(methods::HOST_VOLUMES, "host.volumes");
    // 0.56.0 (#264): connection.list — the daemon's named connections, so a
    // frontend can offer a selector without reading `connections.toml`
    // itself. ONLY `User`, for the same reason as `host.volumes`.
    assert_eq!(methods::CONNECTION_LIST, "connection.list");
    // 0.38.0 (volumes plan task V3.5): `Volume::label` becomes
    // `Option<Vec<u8>>` — a wire fix within the same unreleased branch, the
    // window shifts the same as any bump.
    // 0.39.0 (ADR 0048): fs.compare — comparing two trees as a Task, with
    // its rows by notification.
    assert_eq!(methods::FS_COMPARE, "fs.compare");
    assert_eq!(methods::COMPARE_ROWS, "compare.rows");
    // The LITERALS, not the symbols, for the same reason as
    // `FS_RENAME_BATCH_MAX_PAIRS` above: they are contract that a third
    // party sizes against on its own. The batch cap is the SAME as
    // `search.hits`'s ON PURPOSE — a batch of rows is no more expensive than
    // one of hits —, and the one on entries per directory is what turns an
    // oversized directory into ONE error row instead of an OOM.
    assert_eq!(methods::COMPARE_ROWS_MAX_BATCH, 256);
    assert_eq!(methods::COMPARE_MAX_DIR_ENTRIES, 200_000);
    // 0.40.0 (ADR 0049): sync.plan/apply/report — one-way synchronization as
    // an approvable, retained, undoable plan.
    assert_eq!(methods::SYNC_PLAN, "sync.plan");
    assert_eq!(methods::SYNC_STEPS, "sync.steps");
    assert_eq!(methods::SYNC_PLAN_DONE, "sync.plan_done");
    assert_eq!(methods::SYNC_APPLY, "sync.apply");
    assert_eq!(methods::SYNC_REPORT, "sync.report");
    // The LITERALS again. The TTL is the only one of the four that does not
    // bound a collection: it is the window between approving and executing,
    // and therefore what the executor has to revalidate — a third party that
    // sizes it wrong leaves plans that expire under the mouse. The `include`
    // cap REJECTS (it does not trim), like the rename pairs' does.
    assert_eq!(methods::SYNC_STEPS_MAX_BATCH, 256);
    assert_eq!(methods::SYNC_PLAN_TTL_MS, 600_000);
    assert_eq!(methods::SYNC_MAX_BLOCKERS_REPORTED, 256);
    assert_eq!(methods::SYNC_MAX_INCLUDE, 4096);
    // 0.41.0 (#178): `Error::JournalUnavailable` — an unreadable journal
    // refuses the mutation instead of letting it through unrecorded. A new
    // category, so MINOR: a 0.40.x client degrades it to `Unknown`.
    //
    // 0.42.0 (#170, #152, #195): THREE fields on types that already existed,
    // in one bump — `SyncReportResult::dest_trash`, `SyncFailure::kind` and
    // `CompareRow::paired_under`. Neither a new method nor a new
    // notification, so there is no literal to add above; what changes are
    // the shapes, and `methods.json` and `compare_row.json` pin that.
    //
    // 0.43.0 (#171): `PolicyUndoReportResult` gains `denied`/`denied_total`
    // — the policy is asked unit by unit and INSIDE the Task, so a denial is
    // one report row instead of stopping the whole undo. New fields with
    // `#[serde(default)]`, so a 0.42.x client ignores them and sees the
    // report as always.
    //
    // 0.43.0 (#207): `SyncReason::NonInjectivePairing` — a plan no longer
    // overwrites a pair that only holds up on a transformation that can
    // merge distinct files (the NFC singleton). A new variant of an enum
    // that degrades with `#[serde(other)]`, so MINOR: a 0.42.x client reads
    // it as `Unknown` and paints "a reason this version cannot name" over a
    // step that is ALREADY a `Skip` on the wire — it acts neither less nor more.
    //
    // 0.44.0 (#182): `Error::LIMIT_RETAINED_SYNC_PLANS` — one more token in
    // `LimitExceeded`'s OPEN vocabulary, so the rejection of the retained
    // plans cap travels with taxonomy instead of arriving as "internal
    // error". An N-1 client shows it as is, which is the field's contract.
    // 0.45.0 (#145, #164, ADR 0054): `CapabilityFlags::FULL_FOLD` and
    // `CONFINED_WRITES`, plus `ConflictKind::EscapesRoot`. Both flags are
    // new names in a vocabulary ADR 0004 requires IGNORING when unknown, and
    // the conflict subtype degrades to `Unknown` through ADR 0005's
    // `#[serde(other)]`: a 0.44.x client reads "a conflict this version
    // cannot name" over an operation that failed all the same, it does not
    // act any more than that. MINOR, therefore, and not MAJOR.
    assert_eq!(methods::DAEMON_GOING_AWAY, "daemon.going_away");
    // 0.46.0 (roadmap item 10): `daemon.going_away` and
    // `DaemonShutdownParams.mode`. Both are ADDITIVE and neither changes
    // what was already emitted: `mode` is not serialized when it is `Stop`,
    // so an ordinary stop from a 0.46 client is byte for byte 0.45's
    // message; and a notification an old client does not know is ignored,
    // which is what ADR 0004 requires of it — it is left not knowing a
    // handover was coming and reconnects as always, which is exactly
    // today's behavior. MINOR.
    //
    // `ShutdownMode` does NOT carry `#[serde(other)]`, against this wire's
    // habit: degrading is fine when misreading a value costs a feature, and
    // wrong when it shuts down a daemon in a way nobody asked for.
    // 0.47.0 (roadmap item 11): `rar` in `ARCHIVE_FORMATS`. Widening the
    // whitelist changes no message: it changes which composed schemes can
    // be FORMED. A 0.46 client does not form them and does not see the
    // feature; a 0.47 one against a 0.46 daemon never gets to try, because
    // `version_compatible` does not negotiate a client minor GREATER than
    // the server's. MINOR.
    assert!(norte_proto::ARCHIVE_FORMATS.contains(&"rar"));
    // 0.48.0 (L2): `session.get`/`session.put` and their four types, plus
    // `ConflictKind::StaleRevision` and the `Error::LIMIT_SESSION_BODY`
    // token. Additive: it touches not a single existing message, and the
    // session's body is OPAQUE —the wire freezes that it travels as is, not
    // what it carries inside—. The subtype degrades to `Unknown` through ADR
    // 0005's `#[serde(other)]` and the limit token is OPEN vocabulary that an
    // N-1 client shows as is: both leave the old client with the correct
    // behavior —read again, and do not retry the same body—. MINOR.
    assert_eq!(norte_proto::Error::LIMIT_SESSION_BODY, "session-body");
    // The NAMES, like every other family's: renaming a method is a wire
    // change, and the doctest that shows them is not where this test says it
    // looks.
    assert_eq!(methods::SESSION_GET, "session.get");
    assert_eq!(methods::SESSION_PUT, "session.put");
    // 0.49.0: `fs.dir_size` with its `TaskKind::DirSize` (#139) and
    // `connection.close` (#140). Additive — new methods an old client does
    // not form, and a kind variant its `#[serde(other)]` has degraded to
    // `Unknown` since 0.10. Both go in the SAME bump on purpose: the window
    // moves once per wire release, and this branch has not shipped. MINOR.
    assert_eq!(methods::FS_DIR_SIZE, "fs.dir_size");
    assert_eq!(methods::FS_CHECKSUM, "fs.checksum");
    assert_eq!(methods::FS_CHECKSUM_REPORT, "fs.checksum_report");
    assert_eq!(methods::FS_CHECKSUM_MAX_PATHS, 4096);
    // 0.60.0 (#314). The payload's golden is indexed by the FIXTURE's name,
    // not by this constant, so without these two lines renaming the method
    // passed the whole suite — which is exactly what this test exists to
    // prevent. `MODE_PERMISSION_BITS` is validation contract cited in the
    // field's rustdoc: a client sizes against it.
    assert_eq!(methods::FS_SET_MODE, "fs.set_mode");
    assert_eq!(methods::FS_SET_MODE_MAX_PATHS, 4096);
    assert_eq!(methods::MODE_PERMISSION_BITS, 0o7777);
    // 0.62.0 (#315, #121). Both caps are contract like the ones above, and
    // the first is also of a different class: the others REJECT above their
    // number and this one TRUNCATES, so what the client needs to not
    // misread it is the signal (`TaskProgress::unvisited`), not the number.
    assert_eq!(methods::SET_MODE_RECURSIVE_MAX, 100_000);
    assert_eq!(methods::AI_RENAME_NAMES_MAX, 4096);
    assert_eq!(methods::CONNECTION_CLOSE, "connection.close");
    // 0.50.0: writing archives (#132). Four methods and four new kinds,
    // additive for the same reason as the ones above. None writes INSIDE a
    // container —the archive provider stays `READ_ONLY`, ADR 0018—: all four
    // make new files. Unpacking does not appear because it needs no method:
    // it is an `fs.copy` from the inside, which already existed. MINOR.
    assert_eq!(methods::ARCHIVE_PACK, "archive.pack");
    assert_eq!(methods::ARCHIVE_TEST, "archive.test");
    assert_eq!(methods::FILE_SPLIT, "file.split");
    assert_eq!(methods::FILE_COMBINE, "file.combine");
    // The FIFTH: without this line, renaming `archive.test_report` passed
    // the whole suite. It is the method that collects which entry is
    // corrupt, so its name is contract just like the other four.
    assert_eq!(methods::ARCHIVE_TEST_REPORT, "archive.test_report");
    // And the SIXTH, for the same reason (#250): the goldens freeze the
    // payload's shape, not the method's string, and both the dispatch arm
    // and the SDK cite the constant — so they move together and renaming it
    // passed the whole suite.
    assert_eq!(methods::ARCHIVE_PACK_REPORT, "archive.pack_report");
    // The caps a client can show BEFORE sending anything: 999 chunks is the
    // `.001` convention, and discovering it at chunk 1000 would leave a set
    // nobody can put back together.
    assert_eq!(methods::FILE_SPLIT_MAX_PARTS, 999);
    assert_eq!(methods::FILE_SPLIT_MIN_BYTES, 4096);
    assert_eq!(methods::ARCHIVE_TEST_MAX_FAILURES, 256);
    // 0.51.0 (#247): neither a new type nor a new field — what changed is
    // what `session.put` ACCEPTS (a schema this core cannot read is refused,
    // instead of being written and killing persistence from the next
    // startup on). A bump for wire behavior, which also counts.
    // 0.53.0 (#251, #265, #282): three OPTIONAL fields —a task's unreadable
    // count, a broken plugin's directory bytes, and the anchor a human read
    // when approving—. All three are omitted when there is nothing to say,
    // so an ordinary case's JSON does not change; what shifts the window is
    // that an old peer cannot perform the check each one enables.
    // 0.54.0 (#295): two OPTIONAL fields that are the same datum in both
    // directions —the opaque identity of the directory a listing returned,
    // and the one copy or move return to say "it was that one"—. They are
    // omitted when there is nothing to say, so the ordinary JSON does not
    // change; what shifts the window is that a 0.53 peer cannot perform the
    // check they enable.
    // 0.55.0 (#279): `Error::ApprovalGone`, which says WHICH of the three
    // forms of "that approval is no longer there" happened. Additive —
    // `Error` is `#[non_exhaustive]` and an unknown category falls to
    // `Unknown`—, so what shifts the window is not the JSON but that a 0.54
    // peer will keep counting all three as a generic error.
    // 0.56.0 (#264): `connection.list`. Additive —a method an old client
    // does not call—, and the window still SHIFTS: against a 0.55 daemon
    // there is no connection selector. What is not lost is connecting,
    // which is still navigating to a URL.
    // 0.57.0 (#290): `fs.create`, an EMPTY file as a Task. Additive —new
    // method, new kind that degrades to `Unknown`—, and it shifts the window
    // because against a 0.56 daemon a terminal-less frontend cannot offer
    // "edit a new one": there is no way to create the file.
    // 0.58.0 (#250): `archive.pack_report`, what that packaging saved that
    // MEANS something else outside. Additive —a new method an old client
    // does not call— and the window shifts in the same direction as 0.51.0:
    // a 0.57 client against a 0.58 daemon packages the same and is left
    // without the warning. (Fold collisions do not enter this report: those
    // are REJECTED when packaging, because there a file really does
    // disappear on extraction.)
    // 0.59.0 (#311): `fs.checksum` and its report. Additive —two methods an
    // old client does not call and a kind that degrades to `Unknown`— and
    // here there is no partial degradation at all: against a 0.58 daemon a
    // checksum cannot be checked at all, which is what shifts the window.
    // 0.60.0 (#314): `fs.set_mode`, with its kind and the `POSIX_MODE`
    // capability. Additive, and the window shifts because against a 0.59
    // daemon permissions cannot be changed: the properties surface stays
    // look-only, which is what it was before this version.
    // 0.61.0 (#314): `ApprovalDetail`, and with it the `detail` of both
    // shapes of an approval. Additive —it is omitted when it says nothing—
    // and the window shifts because against a 0.60 daemon a `set-mode`
    // question cannot say WHICH mode, which is half of that decision.
    // 0.62.0 (#315, #121): `recursive`/`dir_mode` in `fs.set_mode` and
    // `names` in `ai.rename_plan`. All three fields are SCOPE —what a
    // request acts on— and all three are omitted when they say nothing, so a
    // client that does not send them changes not a single byte of the JSON.
    // The window shifts because against a 0.61 daemon neither of the two
    // things can be requested: permissions are changed path by path and the
    // AI's plan is for the whole directory.
    // 0.63.0 (#325): `Error::SecretNeeded` and `connection.provide_secret`.
    // The question the core cannot ask on its own —its secret resolver has
    // no user interface and should not have one— going up the wire so
    // whoever is in front answers it, with the same flow as host keys' TOFU.
    // A 0.62 client degrades the error to `Unknown` and shows a failure
    // where the new one opens a dialog: that is what it already did.
    assert_eq!(
        methods::CONNECTION_PROVIDE_SECRET,
        "connection.provide_secret"
    );
    // 0.64.0 (#322): `connection.failed`. A connection failure arrived as a
    // category —`PermissionDenied`, indistinguishable from a wrong
    // password— and the sentence explaining it died in the daemon's log;
    // with the embedded CLI it did get read, meaning the diagnosis depended
    // on the TRANSPORT. It goes by notification because the taxonomy
    // deliberately carries no free text: the error is what decides, and it
    // decides by category. A 0.63 client discards it and stays as it was.
    assert_eq!(methods::CONNECTION_FAILED, "connection.failed");
    // 0.65.0 (#328): `log.tail` and `log.level`. The DAEMON's own log, which
    // a frontend with a separate process cannot see any other way — its
    // panel paints the wrong process's ring, and has said so since #326. It
    // is PULLED with a cursor and not pushed: the daemon keeps no per-client
    // state and the response says how many lines fell off the back, which is
    // what a lost notification cannot say. And raising the level is a METHOD
    // so that the cap stopping an FTP `PASS` from being shown is applied by
    // the only code that can apply it: the one holding the ring.
    assert_eq!(methods::LOG_TAIL, "log.tail");
    assert_eq!(methods::LOG_LEVEL, "log.level");
    // 0.66.0 (D4): no new method — two optional fields, `SpanWire::bg` and
    // `PluginPreviewStyledParams::columns`, for the image previewer that
    // paints half-blocks and needs to know how many cells to shrink into.
    // 0.69.0 (ADR 0100): `plugin.notice`, the notification a `hook` plugin
    // uses to tell the human something about a mutation already recorded —
    // or that the daemon uses to say it turned off a plugin's hooks. Humans
    // only, like `connection.failed`; a 0.68 client discards it.
    assert_eq!(methods::PLUGIN_NOTICE, "plugin.notice");
    // 0.70.0 (ADR 0101): no new method — one more value in
    // `PluginNotice::kind`'s vocabulary, `effect-denied`.
    // 0.71.0 (ADR 0104): `plugin.uninstall`, the extension manager
    // uninstalls without going through the CLI. Humans only, like its siblings.
    assert_eq!(methods::PLUGIN_UNINSTALL, "plugin.uninstall");
    // 0.72.0 (ADR 0105): no new method — `kinds` in `plugin.decorate`'s
    // params and `slot` in each decoration block.
    // 0.73.0 (ADR 0107): `plugin.thumbnail`, a file's thumbnail from a
    // plugin of the new kind. Open, like `plugin.preview`.
    assert_eq!(methods::PLUGIN_THUMBNAIL, "plugin.thumbnail");
    // 0.74.0 (phase 3): `plugin.panel_render`, the frame a `panel`-kind
    // plugin paints into a layout slot. Open, like its twins.
    assert_eq!(methods::PLUGIN_PANEL_RENDER, "plugin.panel_render");
    // 0.75.0 (phase 4): `fs.dir_usage` and its report — what a directory is
    // MADE of, child by child. Two methods because the list does not fit in
    // a Task's outcome, same as `fs.checksum` and its twins; 0.49.0's
    // `fs.dir_size` still answers its own thing, which is a different question.
    assert_eq!(methods::FS_DIR_USAGE, "fs.dir_usage");
    assert_eq!(methods::FS_DIR_USAGE_REPORT, "fs.dir_usage_report");
    // 0.76.0 (phase 7): the journal's timeline. `journal.list` READS it,
    // paging backward by `seq`, and `journal.undo_after` undoes the human's
    // work after a `seq` — with the usual undo, i.e. it reports through
    // `policy.undo_report`. Both, human connections only: the whole journal
    // is an oracle over everything touched on the machine, and undoing work
    // is a decision for whoever did it.
    assert_eq!(methods::JOURNAL_LIST, "journal.list");
    assert_eq!(methods::JOURNAL_UNDO_AFTER, "journal.undo_after");
    // 0.77.0 (phase 8): organizing. The plan is proposed by a model
    // (`ai.organize_plan`) or by an `organizer`-kind plugin
    // (`plugin.organize_plan`), and both answer the SAME type — what makes
    // the operation safe is not where the names came from. Applying it is
    // `fs.organize`, which is one method and not N client calls because
    // creating the directories and moving has to happen under a single
    // `batch_id`: otherwise, undoing the batch gives the files back and
    // forgets the folders.
    assert_eq!(methods::AI_ORGANIZE_PLAN, "ai.organize_plan");
    assert_eq!(methods::PLUGIN_ORGANIZE_PLAN, "plugin.organize_plan");
    assert_eq!(methods::FS_ORGANIZE, "fs.organize");
    // 0.78.0 (phase 9): releasing the UI session without disconnecting,
    // which is what makes the handover between frontends possible. Until
    // now releasing only happened on DISCONNECT, so whoever was leaving had
    // to die before whoever was arriving could claim it.
    assert_eq!(methods::SESSION_RELEASE, "session.release");
    // 0.79.0: no new method — `policy.undo_report` answers the taxonomy's
    // `NotFound` for an id it does not know, like `fs.rename_batch_report`.
    // 0.80.0: `journal.undo_after` gains the optional ceiling `upto_seq`.
    // 0.81.0: no new method — `fs.search` gains ten optional filters, and
    // none of them travels when not requested, so an ordinary search's JSON
    // does not move.
    // 0.82.0: a task can be paused (ADR 0147).
    assert_eq!(methods::TASK_PAUSE, "task.pause");
    assert_eq!(methods::TASK_RESUME, "task.resume");
    // 0.83.0: the serial queue (ADR 0149).
    assert_eq!(methods::TASK_MOVE, "task.move");
    // 0.84.0: no new method. A conflict subtype,
    // `ConflictKind::DestinationGone` (ADR 0151), for when the destination
    // directory stops being where it was requested with the task already
    // under way; a counter in the undo's report, `skipped_not_ours` (ADR
    // 0152, #371); a connection-failure reason, `rsa-too-small` (#370); and
    // `unusable` in `connection.list`, for the entries the daemon failed to
    // read (#365).
    assert_eq!(norte_proto::PROTOCOL_VERSION, "0.84.0");
}

/// An [`Entry`] for a comparison row: the four fields the panel paints,
/// with no attributes (empty ones are omitted).
fn compare_entry(wire: &str, kind: EntryKind, size: Option<u64>, mtime_ms: Option<i64>) -> Entry {
    Entry {
        path: vpath(wire),
        kind,
        size,
        mtime_ms,
        attrs: BTreeMap::new(),
    }
}

/// A row without the three optional fields; whoever needs one fills it in
/// with struct-update syntax.
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

/// The `fs.compare` ROW (0.39.0, ADR 0048), frozen: one fixture per verdict
/// and, across all of them, the WHOLE vocabulary the core ever emits — the
/// six criteria, the three confidences, the five reasons, the two sides and,
/// since 0.42.0, the three pairing transformations
/// ([`compare_row_cases_paired`]). The five `#[serde(other)]` fallbacks have
/// NO fixture on purpose: the core never emits them, so there is no encode
/// direction to pin, and their DECODE degradation is covered by `types.rs`
/// (`unknown_enum_tokens_degrade_and_do_not_error`).
///
/// What these fixtures pin, field by field:
///
/// - `same_size_unknown` is confidence's reason for existing: the right side
///   is INSIDE a zip (`zip+file://…/!/…`, ADR 0018), which gives neither a
///   reliable size nor date. The answer is `Same`/`Unknown`, not an error and
///   not a made-up `Certain`.
/// - `only_left_hostile` (left) and the two `ambiguous_case_fold_hostile*`
///   carry non-UTF8 names (`%FF`, `%FE`): hard rule 1 in both directions, and
///   on BOTH sides of the comparison (`ambiguous_normalization_right` is the
///   right side's).
/// - The three `ambiguous_*` rows freeze the SHAPE that
///   `CompareVerdict::Ambiguous`'s rustdoc declares normative: a collision is
///   of ONE side, so it is ONE row per entry involved, with the other side
///   at `None`. The two in the `case_fold` pair are the two entries that
///   collapse, with DIFFERENT SIZES so it is visible that they are two
///   entries and not one counted twice.
/// - `ambiguous_normalization_right` writes the NFD twin with escapes so no
///   editor can normalize it on its own — the same care as
///   `rename_collision.json`.
/// - `error_dir_too_large_right` carries NO entry from either side: a
///   directory that goes over the cap is reported without having been able
///   to list anything, and that is exactly the shape
///   [`CompareRow::sides_are_consistent`] exempts.
/// - The `criterion` of the problem rows (`ambiguous`, `error`) is
///   `presence` except when a specific rung actually got to run
///   (`error_read_failed_left`, which dies INSIDE the hash). The type has no
///   "no rung" variant and none is invented here: `unknown` is decode's
///   fallback and the core never emits it.
/// - The FIFTEEN rows from 0.39.0 carry no `paired_under` and their JSON did
///   not change a single byte when it was added (0.42.0): the key is
///   omitted when there is no transformation to name, which is the ordinary
///   case. That is what makes the field ADDITIVE, and this file is where it shows.
#[test]
fn golden_compare_row() {
    let mut cases = compare_row_cases_content();
    cases.extend(compare_row_cases_presence());
    cases.extend(compare_row_cases_problems());
    cases.extend(compare_row_cases_paired());

    // Every frozen fixture has to be a LEGAL row: if a golden said `OnlyLeft`
    // while carrying a right side, it would freeze the bug instead of the
    // contract, and task C6's core would be written against it.
    for (name, row) in &cases {
        assert!(row.sides_are_consistent(), "[compare_row/{name}] sides");
        assert!(row.reason_is_consistent(), "[compare_row/{name}] reason");
    }

    check_family("compare_row.json", &cases);
}

/// The pairs whose two names are NOT the same bytes (0.42.0, #152): the
/// verdict is ordinary —`Same` or `Different`, decided by whichever rung ran—
/// and what each fixture freezes is [`CompareRow::paired_under`], which is
/// the only thing saying both halves are spelled differently.
///
/// All three, not one: separating the singleton from the other two IS the
/// contract. A consumer that only saw "they differ in bytes" would have to
/// choose between trusting every pairing by normalization —#152's bug— or
/// rejecting them all, which breaks the macOS↔Linux case the key exists for.
///
/// The KELVIN SIGN and the NFD twin are written with `\u` escapes, for the
/// same reason as `ambiguous_normalization_right`: an editor that normalized
/// the file would turn these tests into tautologies.
fn compare_row_cases_paired() -> Vec<(&'static str, norte_proto::methods::CompareRow)> {
    use norte_proto::methods::CompareConfidence as Conf;
    use norte_proto::methods::CompareCriterion as Crit;
    use norte_proto::methods::CompareVerdict as V;
    use norte_proto::methods::{CompareRow, PairTransform};
    vec![
        // One side does not distinguish case, so the two names CANNOT
        // coexist there and pairing them is the correct call. It travels so
        // a painter can explain why the row shows two spellings.
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
        // The case the key was designed for: the SAME text spelled in NFC by
        // Linux and in NFD by macOS. This is not a warning either; what
        // matters when writing is that the destination is spelled differently.
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
        // #145 in one row: `straße.txt` against `strasse.txt`. Only an
        // ext4/f2fs `+F`'s FULL fold joins them — on any other volume they
        // are two files, and they can be two distinct files. That is why it
        // does NOT share a variant with `case_fold`, whose promise is "the
        // same text written two ways".
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
        // The whole of #152 in one row: U+212A KELVIN SIGN against the ASCII
        // `K`. They coexist on ext4, read as DISTINCT characters, and
        // without this mark a sync plan reads this `Different` as "update
        // the right one with the left one" and writes over a file that has
        // nothing to do with it. The verdict and criterion are the ordinary
        // ones: what is anomalous is not the comparison, it is the PAIR.
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

/// The reference date for the comparison fixtures.
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

/// The rows a CONTENT rung decides (hash, mtime, size, `link_target`): the
/// vocabulary's three confidences come from here, because this is where a
/// criterion proves, suggests, or cannot tell. See [`golden_compare_row`]'s
/// rustdoc.
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
        // Within the default tolerance (2000 ms): `Same`, but only
        // `Probable` — two similar dates do not prove two identical files.
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
        // The field this spec does NOT read and spec 2 needs: which side is
        // newer. It is produced here because it costs nothing.
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
        // Symlinks are compared, not followed: the target is bytes.
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

/// The rows that PRESENCE or type decides, before looking at any content:
/// always `Certain`, because a side that does not exist admits no shades.
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
        // An orphan directory is ONE row and is not enumerated: spec 2's
        // plan will copy it with a recursive `fs.copy`.
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

/// The PROBLEM rows: the two that carry `reason` —`ambiguous` and `error`—
/// and, with them, the five reasons in the closed vocabulary.
fn compare_row_cases_problems() -> Vec<(&'static str, norte_proto::methods::CompareRow)> {
    use norte_proto::methods::CompareConfidence as Conf;
    use norte_proto::methods::CompareCriterion as Crit;
    use norte_proto::methods::CompareReason as Why;
    use norte_proto::methods::CompareVerdict as V;
    use norte_proto::methods::{CompareRow, Side};
    vec![
        // A collision is of ONE side, so the row is too: ONE row per entry
        // involved, with that entry in ITS side's field and the other at
        // `None`. `LEEME%FF.txt` and `leeme%FF.txt` are TWO rows —with
        // different sizes, so it is visible they are not the same entry
        // counted twice— because a sync plan has to see BOTH names before
        // writing over either of them. The shape is frozen in
        // `CompareVerdict::Ambiguous`'s rustdoc (protocol-guardian's MAJOR
        // finding: these fixtures used to carry a crossed pair, which is
        // exactly what the design says is NOT an ambiguity).
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
        // The normalization collision, and on the RIGHT side: the NFD twin
        // of a name that same directory already has in NFC. It is written
        // with escapes so no editor normalizes it on its own — the same
        // care as `rename_collision.json`—, and its NFC twin has its own row
        // exactly like the pair above.
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

/// A step without the three optional fields; whoever needs one fills it in
/// with struct-update syntax.
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

/// The `sync.plan` STEP (0.40.0, ADR 0049), frozen: one fixture per step
/// class and, across all of them, the WHOLE vocabulary the core ever emits —
/// the five classes, the three reversals, the four reasons and the three
/// confidences. The `#[serde(other)]` fallbacks have NO fixture on purpose,
/// for the same reason as in `compare_row.json`: the core never emits them,
/// so there is no encode direction to pin, and their DECODE degradation is
/// covered by `types.rs`.
///
/// What these fixtures pin, field by field:
///
/// - The three `*_trash` / `*_irreversible` pairs are [`StepReversal`]'s
///   reason for existing: the SAME step is worth `restore_trash` or
///   `irreversible` depending on whether the DESTINATION has a trash, and in
///   the second case it owes a reason. A plan that could not tell them apart
///   would promise a human an undo that does not exist.
/// - `copy_hostile` carries a non-UTF8 `rel` (`%FF%FE`): hard rule 1 in both
///   directions. `rel` is RELATIVE to both roots, so it carries neither of them.
/// - `overwrite_dest_spelt_differently` is the pair the pairing key joins and
///   the bytes split: the step carries BOTH paths, because it reads from the
///   one the source spells and writes over the one the destination has. It
///   freezes two things — that `dest_rel` travels ONLY when it differs (in
///   the other ten fixtures the key does not appear) and that what is
///   compared is the WHOLE path, not the last segment.
/// - `overwrite_unknown_confidence` is the `on_unknown: copy` default: it
///   writes, and the step KEEPS `confidence: unknown` so the report can say
///   it copied because nobody could be sure of anything.
///   `skip_unknown_confidence` is the same case with the other choice.
/// - No `skip` carries `reversal`, and all carry `reason`: it is the
///   invariant `shape_is_consistent` states, and it is frozen here as shape.
/// - `delete_tree` carries no `size`: a delete moves no bytes, and the
///   absent key says so better than a zero.
#[test]
fn golden_sync_step() {
    let mut cases = sync_step_cases_acting();
    cases.extend(sync_step_cases_deleting());
    cases.extend(sync_step_cases_skipped());

    // Every frozen fixture has to be a LEGAL step: one that promised an
    // impossible reversal would freeze the bug instead of the contract, and
    // `norte-sync`'s transducer would be written against it.
    for (name, step) in &cases {
        assert!(step.shape_is_consistent(), "[sync_step/{name}] shape");
    }

    check_family("sync_step.json", &cases);
}

/// The steps that WRITE: create, copy and overwrite, with their reversals.
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
            // The SAME row as `overwrite_trash` —same rung, same confidence,
            // same size— against a destination WITHOUT a trash. That the
            // pair varies in nothing else is what turns it into an A/B of
            // the capability instead of two loose examples.
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
            // The pair the pairing key joins and the wire had to be able to
            // name: the source spells the directory `NOTAS` and the
            // destination —which does not distinguish case— has it as
            // `notes`. The step carries BOTH paths; without `dest_rel` the
            // executor would write under the source's and create a second
            // directory alongside it.
            //
            // The difference is in an ANCESTOR and not in the last segment,
            // and that is deliberate for two reasons. One: it freezes the
            // rule this field really implements —the WHOLE path is compared,
            // because the key folds at every level—. Two: the leaf carries
            // byte 0xFF, and a name that is not UTF-8 does NOT fold
            // (`key_for` returns it raw, so as not to spoil Shift-JIS's tail
            // bytes), so a pair that differed only in the case of a non-UTF8
            // leaf does not exist: no walk produces it.
            //
            // The other half of the case —NFC against NFD— is NOT frozen
            // HERE and is also deliberate: both forms are valid UTF-8, so the
            // codec leaves them literal and this fixture would carry two
            // strings that render THE SAME. Such a fixture's failure would be
            // invisible in review. It goes in `types.rs`, with the bytes as escapes.
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

/// The steps that DELETE, which belong to `Mirror` and carry the same pair
/// of reversals: with a trash it comes back out of it, without a trash it
/// comes back from nowhere and the plan says so before anyone approves.
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

/// The steps that touch NOTHING: one per reason, and none with a reversal.
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
            // With no `size`: a `Skip` moves no bytes, and `counts.bytes` is
            // the sum of that field — a size here would be a counted byte
            // nobody wrote, in the number the plan is approved with.
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
            // 0.43.0 (#207): the KELVIN pair. BOTH SPELLINGS travel —`rel`
            // with U+212A and `dest_rel` with the ASCII `K`— because they are
            // the row's whole point: whoever reads it has to be able to see
            // that the two names are NOT the same text. With no `size`, like
            // every `Skip`.
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

/// The BLOCKER (0.40.0, ADR 0049): the five classes, and `side` present
/// exactly when the blocker is of one side. `dest_read_only` is of the WHOLE
/// tree, so its `rel` is the ROOT — the shape a frontend has to know how to
/// paint with no name to show.
///
/// `type_mismatch_dir` appears TWICE because its `side` is what makes it
/// legible: the same blocker with `left` and with `right` are two distinct
/// sentences ("I will not copy a source tree over a file" / "I will not
/// delete a destination tree to put a file there"), and freezing only one
/// would leave the other with no fixture.
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
                // 0.52.0 (#163): a name the destination cannot have. The
                // side is ALWAYS the destination — it is its filesystem that
                // refuses it, not the source that wrote it wrong.
                "illegal_dest_name",
                SyncBlocker {
                    rel: rel_path("CON"),
                    kind: Kind::IllegalDestName,
                    side: Some(Side::Right),
                },
            ),
            (
                // The overlap is of BOTH roots at once: there is no side to
                // name, and `side` is omitted instead of inventing one.
                "overlap_detected",
                SyncBlocker {
                    rel: rel_path("sub"),
                    kind: Kind::OverlapDetected,
                    side: None,
                },
            ),
            (
                // A SOURCE directory against a destination file, with a
                // non-UTF8 name so the blocker's `rel` goes through the same
                // codec as a step's.
                "type_mismatch_dir_source",
                SyncBlocker {
                    rel: rel_path("informe%FF.d"),
                    kind: Kind::TypeMismatchDir,
                    side: Some(Side::Left),
                },
            ),
            (
                // And the other way around: the tree that would be deleted
                // is in the DESTINATION.
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
        // u64::MAX: where a JS client loses the value in its f64.
        ("uint_max", AttrValue::Uint(u64::MAX)),
        ("int", AttrValue::Int(-7)),
        ("text", AttrValue::Text("STANDARD_IA".to_owned())),
        // Bytes that are NOT UTF-8: the variant's reason for existing.
        ("bytes_b64", AttrValue::Bytes(vec![0xFF, 0xFE])),
        // Empty is NOT absent: the cell exists and its value is zero bytes.
        ("bytes_b64_empty", AttrValue::Bytes(Vec::new())),
        // Negative: pre-1970 is real and the wire allows it.
        ("time_ms", AttrValue::TimeMs(-86_400_000)),
        ("bool", AttrValue::Bool(true)),
        ("unknown", AttrValue::Unknown),
    ];
    // Exhaustiveness: `attr_value_tag` is a `match` with no wildcard, so a
    // NEW variant breaks compilation until someone covers it; this set-check
    // turns "I added the variant, forgot the fixture" into red.
    let covered: BTreeSet<&str> = attr_values.iter().map(|(_, v)| attr_value_tag(v)).collect();
    let all: BTreeSet<&str> = ATTR_VALUE_TAGS.into_iter().collect();
    assert_eq!(
        covered, all,
        "[attr_value.json] every AttrValue variant needs at least one fixture"
    );
    check_family("attr_value.json", &attr_values);
}

/// All of [`AttrValue`]'s wire tags, cross-checked against
/// [`attr_value_tag`]'s exhaustive `match`.
const ATTR_VALUE_TAGS: [&str; 7] = [
    "uint",
    "int",
    "text",
    "bytes_b64",
    "time_ms",
    "bool",
    "unknown",
];

/// A value's wire tag. EXHAUSTIVE by construction (no `_`): adding a variant
/// to `AttrValue` breaks compilation here.
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

/// `host.*` family (0.37.0/0.38.0, #131): enumerating the host's volumes.
/// `Volume::mount` is a [`VPath`] — `volume_hostile_no_sizes` uses a NON-UTF8
/// mount point to demonstrate the byte-for-byte round trip (hard rule 1),
/// and the SAME fixture pins the "no sizes" shape: `total_bytes`/
/// `free_bytes` absent from the wire, never a zero disguised as "unknown"
/// (design §A of `2026-08-10-volumes-design.md`).
///
/// V3.5 (0.38.0): `Volume::label` is `Option<Vec<u8>>`, base64 on the wire —
/// `volume` pins the ordinary case (`"USB Nico"` encoded), and the SAME
/// `volume_hostile_no_sizes` fixture that already carried the non-UTF8 mount
/// ALSO gains a non-UTF8 label (`\xFF\xFE`), so a single fixture demonstrates
/// that neither of the type's two byte fields goes through a `String`.
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

    // Non-UTF8 mount point AND label + the "no sizes" shape — see the
    // function's comment.
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

    // Decode-only (asymmetric, like `plugin_help_result_absent`): a `kind`
    // this client does not know degrades to `Unknown` through
    // `#[serde(other)]` instead of throwing away the whole `host.volumes`
    // response — the core NEVER emits this value, so there is no encode
    // direction to pin.
    let future_kind: Volume = serde_json::from_value(
        fixtures
            .get("volume_future_kind")
            .expect("[methods.json] missing the volume_future_kind fixture")
            .clone(),
    )
    .expect("[methods/volume_future_kind] deserialize");
    assert_eq!(
        future_kind.kind,
        VolumeKind::Unknown,
        "an unknown kind degrades to Unknown, it does not break decoding"
    );

    // Decode-only, with no fixture registered (protocol-guardian MINOR, V3.5
    // review): `label` with unreadable base64 degrades THAT FIELD to `None`,
    // not the whole `Volume` — the contract `label_wire`'s rustdoc promises.
    // `mount`/`fs_type` stay intact, which is exactly what demonstrates that
    // the rest of the entry was not lost along with the bad field.
    let bad_label_json = serde_json::json!({
        "mount": "file:///media/usb",
        "label": "this is not base64 !!",
        "fs_type": "vfat",
        "kind": "removable",
        "read_only": false
    });
    let bad_label: Volume =
        serde_json::from_value(bad_label_json).expect("[methods/bad label] deserialize");
    assert_eq!(bad_label.label, None, "unreadable base64 degrades to None");
    assert_eq!(bad_label.mount, vpath("file:///media/usb"));
    assert_eq!(bad_label.fs_type, "vfat");

    // Same contract for a DECODABLE payload that is over the cap
    // (`ATTR_BYTES_MAX`, reused — see `label_wire`'s rustdoc).
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
        serde_json::from_value(oversized_json).expect("[methods/oversized label] deserialize");
    assert_eq!(
        oversized_label.label, None,
        "a label decoded over the cap also degrades to None"
    );
}
