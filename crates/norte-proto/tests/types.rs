//! Tests deterministas de los tipos del protocolo (fase 4 de M0): serde
//! roundtrip, tolerancia a campos desconocidos (forward-compat N/N-1) y
//! semántica de cada tipo. El wire byte-exacto vive en `golden_types.rs`.

use norte_proto::{
    Capabilities, CapabilityFlags, ConflictKind, Entry, EntryKind, Error, TaskId, TaskKind,
    TaskProgress, TaskState, VPath,
};

fn vpath(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido de test")
}

fn roundtrip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_string(value).expect("serializable");
    serde_json::from_str(&json).expect("deserializable")
}

// ---------- Entry ----------

fn sample_entry() -> Entry {
    Entry {
        path: vpath("file:///home/user/doc.txt"),
        kind: EntryKind::File,
        size: Some(1234),
        mtime_ms: Some(1_720_000_000_000),
    }
}

#[test]
fn entry_roundtrip() {
    let e = sample_entry();
    assert_eq!(roundtrip(&e), e);
}

#[test]
fn entry_none_fields_roundtrip() {
    let e = Entry {
        path: vpath("file:///dir"),
        kind: EntryKind::Dir,
        size: None,
        mtime_ms: None,
    };
    assert_eq!(roundtrip(&e), e);
}

#[test]
fn entry_tolerates_unknown_fields() {
    // Forward-compat: un core N+1 puede añadir campos; un cliente N no revienta.
    let json = r#"{
        "path": "file:///a",
        "kind": "file",
        "size": 1,
        "mtime_ms": null,
        "campo_del_futuro": {"x": 1}
    }"#;
    let e: Entry = serde_json::from_str(json).expect("campos desconocidos se ignoran");
    assert_eq!(e.kind, EntryKind::File);
}

#[test]
fn entry_optional_fields_default() {
    // Backward-compat: campos opcionales ausentes → None, no error.
    let json = r#"{"path": "file:///a", "kind": "other"}"#;
    let e: Entry = serde_json::from_str(json).expect("opcionales ausentes valen None");
    assert_eq!(e.size, None);
    assert_eq!(e.mtime_ms, None);
}

#[test]
fn entry_mtime_pre_epoch() {
    // mtime_ms es i64: fechas pre-1970 existen en FS reales.
    let e = Entry {
        mtime_ms: Some(-86_400_000),
        ..sample_entry()
    };
    assert_eq!(roundtrip(&e), e);
}

#[test]
fn entry_kind_wire_strings() {
    for (kind, wire) in [
        (EntryKind::File, "\"file\""),
        (EntryKind::Dir, "\"dir\""),
        (EntryKind::Symlink, "\"symlink\""),
        (EntryKind::Other, "\"other\""),
    ] {
        assert_eq!(serde_json::to_string(&kind).unwrap(), wire);
    }
}

// ---------- Capabilities ----------

#[test]
fn capabilities_roundtrip() {
    let c = Capabilities {
        flags: CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_SENSITIVE,
        max_path: Some(4096),
    };
    assert_eq!(roundtrip(&c), c);
}

#[test]
fn capabilities_empty_roundtrip() {
    let c = Capabilities {
        flags: CapabilityFlags::empty(),
        max_path: None,
    };
    assert_eq!(roundtrip(&c), c);
}

#[test]
fn capabilities_unknown_flag_names_ignored() {
    // Política ADR 0004: una capability es un anuncio — un nombre N+1 bien
    // formado se ignora (no se explota), jamás revienta al cliente N.
    let json = r#"{"flags": "RENAME_ATOMIC | FLAG_DEL_FUTURO", "max_path": null}"#;
    let c: Capabilities = serde_json::from_str(json).expect("nombre desconocido se ignora");
    assert_eq!(c.flags, CapabilityFlags::RENAME_ATOMIC);
}

#[test]
fn capabilities_hex_bits_rejected() {
    // Bits sin nombre NO viajan: bitflags::parser::from_str los retendría en
    // silencio vía hex; el wire los rechaza siempre.
    for bad in ["0x20", "0x3", "RENAME_ATOMIC | 0x40", "0X20"] {
        let json = format!(r#"{{"flags": "{bad}", "max_path": null}}"#);
        assert!(
            serde_json::from_str::<Capabilities>(&json).is_err(),
            "hex debía fallar: {bad}"
        );
    }
}

#[test]
fn capabilities_malformed_flags_rejected() {
    for bad in [
        "RENAME_ATOMIC |",
        "| SYMLINKS",
        "###",
        "rename_atomic",
        "A B",
    ] {
        let json = format!(r#"{{"flags": "{bad}", "max_path": null}}"#);
        assert!(
            serde_json::from_str::<Capabilities>(&json).is_err(),
            "malformado debía fallar: {bad}"
        );
    }
}

// ---------- TaskId / TaskKind / TaskState ----------

#[test]
fn task_id_is_transparent_number() {
    let id = TaskId::new(42);
    assert_eq!(serde_json::to_string(&id).unwrap(), "42");
    assert_eq!(serde_json::from_str::<TaskId>("42").unwrap(), id);
    assert_eq!(id.get(), 42);
}

#[test]
fn task_state_roundtrip_all() {
    let states = [
        TaskState::Pending,
        TaskState::Running,
        TaskState::Paused,
        TaskState::Completed,
        TaskState::Cancelled,
        TaskState::Failed {
            error: Error::ProviderUnavailable { retryable: true },
        },
    ];
    for s in states {
        assert_eq!(roundtrip(&s), s);
    }
}

#[test]
fn task_state_terminal() {
    assert!(!TaskState::Pending.is_terminal());
    assert!(!TaskState::Running.is_terminal());
    assert!(!TaskState::Paused.is_terminal());
    assert!(TaskState::Completed.is_terminal());
    assert!(TaskState::Cancelled.is_terminal());
    assert!(
        TaskState::Failed {
            error: Error::Cancelled
        }
        .is_terminal()
    );
}

#[test]
fn task_state_unknown_kind_degrades() {
    // Tolerancia N/N-1: estado desconocido → Unknown, NO terminal (el cliente
    // sigue escuchando), jamás error de deserialización.
    let s: TaskState = serde_json::from_str(r#"{"kind": "estado_del_futuro"}"#).unwrap();
    assert!(!s.is_terminal());
    // Con payload extra también.
    let s: TaskState =
        serde_json::from_str(r#"{"kind": "estado_del_futuro", "detalle": 5}"#).unwrap();
    assert!(!s.is_terminal());
}

#[test]
fn task_kind_wire_strings() {
    for (kind, wire) in [
        (TaskKind::Copy, "\"copy\""),
        (TaskKind::Move, "\"move\""),
        (TaskKind::Delete, "\"delete\""),
    ] {
        assert_eq!(serde_json::to_string(&kind).unwrap(), wire);
    }
}

// ---------- TaskProgress ----------

#[test]
fn task_progress_roundtrip() {
    let p = TaskProgress {
        task_id: TaskId::new(7),
        kind: TaskKind::Copy,
        state: TaskState::Running,
        bytes_done: 1024,
        bytes_total: Some(4096),
        entries_done: 1,
        entries_total: Some(3),
        current: Some(vpath("file:///a/b")),
    };
    assert_eq!(roundtrip(&p), p);
}

#[test]
fn task_progress_unknown_totals() {
    // Totales desconocidos (walk aún en curso): None, jamás 0 fingido.
    let p = TaskProgress {
        task_id: TaskId::new(1),
        kind: TaskKind::Delete,
        state: TaskState::Pending,
        bytes_done: 0,
        bytes_total: None,
        entries_done: 0,
        entries_total: None,
        current: None,
    };
    assert_eq!(roundtrip(&p), p);
}

// ---------- Error (§17.7) ----------

#[test]
fn error_roundtrip_all_variants() {
    let errors = [
        Error::NotFound,
        Error::PermissionDenied,
        Error::Conflict {
            conflict: ConflictKind::Exists,
        },
        Error::Conflict {
            conflict: ConflictKind::CaseCollision,
        },
        Error::Conflict {
            conflict: ConflictKind::Normalization,
        },
        Error::Conflict {
            conflict: ConflictKind::TypeMismatch,
        },
        Error::ProviderUnavailable { retryable: false },
        Error::NoSpace,
        Error::Io { retryable: true },
        Error::Io { retryable: false },
        Error::Cancelled,
        Error::PolicyDenied {
            rule: "no_delete_home".to_owned(),
        },
        Error::EncodingLoss,
        Error::Unsupported,
        Error::InvalidPath,
        Error::Internal { panic: true },
    ];
    for e in errors {
        assert_eq!(roundtrip(&e), e);
    }
}

#[test]
fn error_unknown_kind_degrades() {
    // Tolerancia N/N-1: categoría desconocida → error genérico, no reventón.
    let e: Error = serde_json::from_str(r#"{"kind": "quota_del_futuro"}"#).unwrap();
    assert_eq!(e, Error::Unknown);
    let with_payload: Error =
        serde_json::from_str(r#"{"kind": "quota_del_futuro", "limite": 9}"#).unwrap();
    assert_eq!(with_payload, Error::Unknown);
}

#[test]
fn conflict_unknown_subtype_degrades_nested() {
    // Tolerancia N/N-1 (ADR 0005): un subtipo de conflicto desconocido
    // DENTRO de un Error::Conflict conocido degrada a Unknown, no revienta.
    let e: Error =
        serde_json::from_str(r#"{"kind": "conflict", "conflict": "subtipo_del_futuro"}"#).unwrap();
    assert_eq!(
        e,
        Error::Conflict {
            conflict: ConflictKind::Unknown
        }
    );
}

#[test]
fn unknown_policies_are_hard_errors() {
    // Asimetría deliberada (ADR 0005): las políticas viajan client→server
    // como ÓRDENES mutantes — un core que no las entiende debe rechazar el
    // request, jamás degradar a un default que haga otra cosa.
    assert!(
        serde_json::from_str::<norte_proto::CollisionPolicy>(r#""politica_del_futuro""#).is_err()
    );
    assert!(
        serde_json::from_str::<norte_proto::SymlinkPolicy>(r#""politica_del_futuro""#).is_err()
    );
}

#[test]
fn copy_params_absent_policies_default() {
    // La forma de wire 0.1.0 ({"from","to"} sin políticas) sigue siendo
    // válida: ausencia = Fail/Preserve (el comportamiento de M0).
    use norte_proto::methods::{FsCopyParams, FsMoveParams};
    let p: FsCopyParams =
        serde_json::from_str(r#"{"from": "file:///a", "to": "file:///b"}"#).unwrap();
    assert_eq!(p.on_collision, norte_proto::CollisionPolicy::Fail);
    assert_eq!(p.symlinks, norte_proto::SymlinkPolicy::Preserve);
    let m: FsMoveParams =
        serde_json::from_str(r#"{"from": "file:///a", "to": "file:///b"}"#).unwrap();
    assert_eq!(m.on_collision, norte_proto::CollisionPolicy::Fail);
    assert_eq!(m.symlinks, norte_proto::SymlinkPolicy::Preserve);
}

#[test]
fn error_display_is_english_and_stable() {
    // Display es para logs (los frontends renderizan por categoría, no por string).
    assert_eq!(Error::NotFound.to_string(), "not found");
    assert_eq!(Error::Cancelled.to_string(), "cancelled");
    assert!(
        Error::ProviderUnavailable { retryable: true }
            .to_string()
            .contains("retryable")
    );
}

#[test]
fn error_is_std_error() {
    fn assert_err<E: std::error::Error>(_: &E) {}
    assert_err(&Error::NotFound);
}
