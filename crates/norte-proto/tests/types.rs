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
        (TaskKind::Undo, "\"undo\""),
        (TaskKind::Search, "\"search\""),
        (TaskKind::Index, "\"index\""),
    ] {
        assert_eq!(serde_json::to_string(&kind).unwrap(), wire);
    }
}

#[test]
fn task_kind_unknown_is_tolerant() {
    // Un cliente N-1 recibe un kind futuro → Unknown, no error de parse
    // (forward-compat, igual que TaskState::Unknown).
    let k: TaskKind = serde_json::from_str("\"teleport\"").expect("tolerante");
    assert_eq!(k, TaskKind::Unknown);
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
    // Resume/verify (0.6.0) son igual de mutantes: valor desconocido = error.
    assert!(serde_json::from_str::<norte_proto::ResumePolicy>(r#""futuro""#).is_err());
    assert!(serde_json::from_str::<norte_proto::VerifyPolicy>(r#""futuro""#).is_err());
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
    // resume/verify ausentes (cliente 0.5) = Off/Length = contrato M1 (0.6.0).
    assert_eq!(p.resume, norte_proto::ResumePolicy::Off);
    assert_eq!(p.verify, norte_proto::VerifyPolicy::Length);
    let m: FsMoveParams =
        serde_json::from_str(r#"{"from": "file:///a", "to": "file:///b"}"#).unwrap();
    assert_eq!(m.on_collision, norte_proto::CollisionPolicy::Fail);
    assert_eq!(m.symlinks, norte_proto::SymlinkPolicy::Preserve);
    assert_eq!(m.resume, norte_proto::ResumePolicy::Off);
    assert_eq!(m.verify, norte_proto::VerifyPolicy::Length);
}

#[test]
fn delete_params_sin_mode_es_trash() {
    // ADR 0009: el default del wire es el SEGURO — un cliente 0.2 que no
    // manda mode obtiene papelera, jamás pérdida sorpresa.
    use norte_proto::methods::FsDeleteParams;
    let p: FsDeleteParams = serde_json::from_str(r#"{"path": "file:///x"}"#).unwrap();
    assert_eq!(p.mode, norte_proto::DeleteMode::Trash);
    // Un modo desconocido es error duro (orden mutante, como las políticas).
    assert!(
        serde_json::from_str::<FsDeleteParams>(r#"{"path": "file:///x", "mode": "modo_futuro"}"#)
            .is_err()
    );
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

// ---------- envelope JSON-RPC (ADR 0011) ----------

/// Tolerancia de structs (ADR 0004) aplica al envelope: campos extra de un
/// protocolo más nuevo se ignoran.
#[test]
fn envelope_ignora_campos_desconocidos() {
    use norte_proto::wire::{Message, Request};
    let r: Request = serde_json::from_str(
        r#"{"jsonrpc":"2.0","id":1,"method":"fs.list","params":null,"traceparent":"00-abc"}"#,
    )
    .unwrap();
    assert_eq!(r.method, "fs.list");
    // Y la clasificación estructural no se despista por el campo extra.
    let m: Message = serde_json::from_str(
        r#"{"jsonrpc":"2.0","id":1,"method":"fs.list","params":null,"extra":1}"#,
    )
    .unwrap();
    assert!(matches!(m, Message::Request(_)));
}

/// `jsonrpc` distinto de "2.0" se RECHAZA (peer que no habla el protocolo).
#[test]
fn envelope_rechaza_jsonrpc_distinto_de_2_0() {
    use norte_proto::wire::Request;
    for raw in [
        r#"{"jsonrpc":"1.0","id":1,"method":"m","params":null}"#,
        r#"{"jsonrpc":"3.0","id":1,"method":"m","params":null}"#,
        r#"{"id":1,"method":"m","params":null}"#,
    ] {
        assert!(
            serde_json::from_str::<Request>(raw).is_err(),
            "debía rechazar: {raw}"
        );
    }
}

/// Una response con result Y error (o ninguno) viola JSON-RPC: `outcome`
/// la convierte en error de protocolo, jamás la interpreta.
#[test]
fn response_outcome_valida_xor() {
    use norte_proto::wire::{Response, RpcError, codes};
    let both: Response = serde_json::from_str(
        r#"{"jsonrpc":"2.0","id":1,"result":{},"error":{"code":-32000,"message":"x","data":null}}"#,
    )
    .unwrap();
    assert_eq!(both.outcome().unwrap_err().code, codes::INVALID_REQUEST);
    let neither: Response =
        serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":null,"error":null}"#).unwrap();
    assert_eq!(neither.outcome().unwrap_err().code, codes::INVALID_REQUEST);
    // El error del peer se entrega tal cual.
    let err = Response::err(None, RpcError::protocol(codes::PARSE_ERROR, "x"));
    assert_eq!(err.outcome().unwrap_err().code, codes::PARSE_ERROR);
}

/// El error de aplicación lleva la taxonomía ÍNTEGRA en data — el contrato
/// de los frontends es data.kind, no code/message.
#[test]
fn rpc_error_de_aplicacion_lleva_la_taxonomia_en_data() {
    use norte_proto::wire::{RpcError, codes};
    let e = RpcError::from(Error::Conflict {
        conflict: ConflictKind::CaseCollision,
    });
    assert_eq!(e.code, codes::APP_ERROR);
    assert_eq!(
        e.data,
        Some(Error::Conflict {
            conflict: ConflictKind::CaseCollision
        })
    );
    // data de un protocolo más nuevo degrada por el fallback de Error.
    let raw = r#"{"code":-32000,"message":"x","data":{"kind":"categoria_del_futuro"}}"#;
    let back: RpcError = serde_json::from_str(raw).unwrap();
    assert_eq!(back.data, Some(Error::Unknown));
}

// ---------- framing NDJSON (ADR 0011) ----------

#[test]
fn frame_decoder_trocea_y_tolera() {
    use norte_proto::wire::FrameDecoder;
    let mut d = FrameDecoder::new();
    // Parcial, luego dos completos en un push, con \r\n y línea vacía.
    d.push(b"{\"a\"").unwrap();
    assert_eq!(d.next_frame(), None);
    d.push(b":1}\r\n\n{\"b\":2}\n{\"c\"").unwrap();
    assert_eq!(d.next_frame(), Some(b"{\"a\":1}".to_vec()));
    assert_eq!(d.next_frame(), Some(b"{\"b\":2}".to_vec()));
    assert_eq!(d.next_frame(), None, "el tercero no cerró");
    d.push(b":3}\n").unwrap();
    assert_eq!(d.next_frame(), Some(b"{\"c\":3}".to_vec()));
}

#[test]
fn frame_decoder_rechaza_frames_gigantes() {
    use norte_proto::wire::{FrameDecoder, MAX_FRAME_BYTES};
    let mut d = FrameDecoder::new();
    let chunk = vec![b'x'; 1024 * 1024];
    let mut fallo = false;
    for _ in 0..=(MAX_FRAME_BYTES / chunk.len()) {
        if d.push(&chunk).is_err() {
            fallo = true;
            break;
        }
    }
    assert!(fallo, "un frame sin fin debe cortarse en MAX_FRAME_BYTES");
}

#[test]
fn encode_frame_termina_en_newline_y_roundtripea() {
    use norte_proto::wire::{FrameDecoder, Request, RequestId, encode_frame};
    let req = Request {
        jsonrpc: norte_proto::wire::JsonRpcVersion,
        id: RequestId::Num(1),
        method: "fs.stat".into(),
        params: None,
    };
    let frame = encode_frame(&req).unwrap();
    assert_eq!(frame.last(), Some(&b'\n'));
    // serde_json escapa \n internos: un frame es SIEMPRE una línea.
    assert_eq!(
        frame.iter().position(|&b| b == b'\n'),
        Some(frame.len() - 1)
    );
    let mut d = FrameDecoder::new();
    d.push(&frame).unwrap();
    let back: Request = serde_json::from_slice(&d.next_frame().unwrap()).unwrap();
    assert_eq!(back, req);
}

// ---------- versionado N/N-1 (ADR 0011) ----------

#[test]
fn version_compatible_solo_n_y_n_menos_1() {
    use norte_proto::methods::version_compatible;
    // 0.x: el minor es el major efectivo.
    assert!(version_compatible("0.4.0", "0.4.7"));
    assert!(version_compatible("0.4.0", "0.3.2"));
    assert!(!version_compatible("0.4.0", "0.2.9"));
    assert!(!version_compatible("0.4.0", "0.5.0"));
    assert!(!version_compatible("0.4.0", "1.4.0"));
    // Malformados: jamás compatibles, jamás panic.
    for v in ["", "0.4", "0.4.0.1", "a.b.c", "0.4.x", " 0.4.0"] {
        assert!(!version_compatible("0.4.0", v), "aceptó {v:?}");
    }
}

/// Tolerancia del envelope: `params` AUSENTE (no null) y `encodings`
/// ausente en initialize — el receptor acepta ausencia (ADR 0004).
#[test]
fn envelope_tolera_ausencias() {
    use norte_proto::methods::InitializeParams;
    use norte_proto::wire::{Notification, Request};
    let r: Request = serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"m"}"#).unwrap();
    assert_eq!(r.params, None);
    let n: Notification = serde_json::from_str(r#"{"jsonrpc":"2.0","method":"m"}"#).unwrap();
    assert_eq!(n.params, None);
    let p: InitializeParams = serde_json::from_str(
        r#"{"client_info":{"name":"x","version":"0"},"protocol_version":"0.4.0"}"#,
    )
    .unwrap();
    assert!(p.encodings.is_empty(), "encodings ausente = vacío = json");
}

/// Clasificación estructural (M2/M3 del guardian): JSON válido que no es
/// envelope, y requests con id de tipo ilegal — jamás silencio.
#[test]
fn classify_distingue_lo_invalido_de_lo_ilegal() {
    use norte_proto::wire::{MessageKind, classify};
    let j = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
    assert_eq!(classify(&j(r#"{"foo":1}"#)), MessageKind::Invalid);
    assert_eq!(classify(&j("[1,2]")), MessageKind::Invalid);
    // id ilegal (negativo/fraccional/null): sigue siendo Request — el
    // server responde INVALID_REQUEST en vez de tragárselo.
    assert_eq!(
        classify(&j(r#"{"jsonrpc":"2.0","id":-1,"method":"m"}"#)),
        MessageKind::Request
    );
    assert_eq!(
        classify(&j(r#"{"jsonrpc":"2.0","id":null,"method":"m"}"#)),
        MessageKind::Request
    );
    assert_eq!(
        classify(&j(r#"{"jsonrpc":"2.0","method":"m"}"#)),
        MessageKind::Notification
    );
    assert_eq!(
        classify(&j(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#)),
        MessageKind::Response
    );
    // Response con error y sin id explícito también clasifica.
    assert_eq!(
        classify(&j(
            r#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"x"}}"#
        )),
        MessageKind::Response
    );
}

/// Semver estricto en la negociación (m2 del guardian): ni `+`, ni ceros a
/// la izquierda, ni pre-release/build metadata — deliberado y pinneado.
#[test]
fn version_compatible_es_estricta_con_el_formato() {
    use norte_proto::methods::version_compatible;
    for v in ["+0.4.0", "0.04.0", "0.4.00", "0.4.0-rc.1", "0.4.0+abc"] {
        assert!(!version_compatible("0.4.0", v), "aceptó {v:?}");
    }
}

/// Tolerancia (ADR 0004): `range` AUSENTE en fs.read = None (default),
/// no solo `null` explícito (ADR 0004).
#[test]
fn fs_read_params_tolera_range_ausente() {
    use norte_proto::methods::FsReadParams;
    let p: FsReadParams = serde_json::from_str(r#"{"path":"file:///x"}"#).expect("range ausente");
    assert!(p.range.is_none());
}

/// Compat N-1 (ADR 0017): un cliente 0.7 OMITE `limit`/`cursor`/`next_cursor`
/// (no los manda `null`). El golden pinnea el `null` canónico del emisor 0.8;
/// esto pinnea la otra dirección — claves AUSENTES → None. Sin esto, quitar el
/// `#[serde(default)]` pasaría todos los tests y solo rompería a los 0.7.
#[test]
fn fs_list_params_tolera_cursor_y_limit_ausentes() {
    use norte_proto::methods::FsListParams;
    let p: FsListParams =
        serde_json::from_str(r#"{"path":"file:///x"}"#).expect("limit/cursor ausentes");
    assert!(p.limit.is_none() && p.cursor.is_none());
}

#[test]
fn fs_list_result_tolera_next_cursor_ausente() {
    use norte_proto::methods::FsListResult;
    let r: FsListResult = serde_json::from_str(r#"{"entries":[]}"#).expect("next_cursor ausente");
    assert!(r.next_cursor.is_none());
    // `skipped` (0.22, #93) ausente = None; y un emisor 0.22 lo OMITE cuando
    // es None (skip_serializing_if — jamás `"skipped": null` en el wire).
    assert!(r.skipped.is_none());
    assert!(
        !serde_json::to_string(&r)
            .expect("serializable")
            .contains("skipped")
    );
    // Y un campo DESCONOCIDO (0.9 → 0.8) no rompe la deserialización.
    let r2: FsListResult = serde_json::from_str(r#"{"entries":[],"campo_futuro":42}"#)
        .expect("campo desconocido tolerado");
    assert!(r2.entries.is_empty());
}

#[test]
fn agent_session_optional_roundtrip() {
    use norte_proto::methods::InitializeParams;
    // Ausente = None (frontend humano); un cliente 0.10 no lo envía.
    let humano: InitializeParams = serde_json::from_str(
        r#"{"client_info":{"name":"tui","version":"1"},"protocol_version":"0.11.0"}"#,
    )
    .expect("sin agent_session");
    assert_eq!(humano.agent_session, None);
    // Presente = sesión de agente.
    let agente = InitializeParams {
        client_info: norte_proto::methods::ClientInfo {
            name: "mcp".into(),
            version: "1".into(),
        },
        protocol_version: "0.11.0".into(),
        encodings: vec![],
        agent_session: Some("s1".into()),
    };
    let wire = serde_json::to_string(&agente).unwrap();
    assert!(wire.contains("\"agent_session\":\"s1\""));
    let back: InitializeParams = serde_json::from_str(&wire).unwrap();
    assert_eq!(back.agent_session.as_deref(), Some("s1"));
}

#[test]
fn policy_types_roundtrip() {
    use norte_proto::methods::{
        PolicyApprovalRequired, PolicyDecideParams, RequestScopeParams, RequestScopeResult,
    };
    let req = RequestScopeParams {
        session: "s1".into(),
        roots: vec![VPath::parse("file:///work").unwrap()],
        ops: vec!["copy".into(), "delete".into()],
        ttl_ms: 60_000,
    };
    let back: RequestScopeParams =
        serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
    assert_eq!(back, req);
    assert_eq!(
        serde_json::from_str::<RequestScopeResult>(r#"{"request_id":3}"#)
            .unwrap()
            .request_id,
        3
    );
    let ar = PolicyApprovalRequired {
        approval_id: 7,
        session: Some("s1".into()),
        op: "delete".into(),
        paths: vec!["file:///work/x".into()],
        ttl_ms: 30_000,
    };
    let back: PolicyApprovalRequired =
        serde_json::from_str(&serde_json::to_string(&ar).unwrap()).unwrap();
    assert_eq!(back, ar);
    let dec: PolicyDecideParams =
        serde_json::from_str(r#"{"approval_id":7,"approve":true}"#).unwrap();
    assert!(dec.approve);
}

#[test]
fn version_ventana_actual() {
    use norte_proto::PROTOCOL_VERSION;
    use norte_proto::methods::version_compatible;
    // 0.29.0 (#101): acepta 0.29.x (N) y 0.28.x (N-1), rechaza 0.27.x (N-2).
    assert!(version_compatible(PROTOCOL_VERSION, "0.29.9"), "N");
    assert!(version_compatible(PROTOCOL_VERSION, "0.28.0"), "N-1");
    assert!(
        !version_compatible(PROTOCOL_VERSION, "0.27.9"),
        "N-2 fuera de la ventana"
    );
}

#[test]
fn connection_degraded_round_trip_y_detail_omitido() {
    use norte_proto::methods::ConnectionDegraded;
    let sin = ConnectionDegraded {
        scheme: "ftp".into(),
        host: "h".into(),
        reason: "tls-auth-rejected".into(),
        detail: None,
    };
    assert_eq!(
        serde_json::to_value(&sin).unwrap(),
        serde_json::json!({"scheme":"ftp","host":"h","reason":"tls-auth-rejected"}),
    );
    let con = ConnectionDegraded {
        detail: Some("server rejected AUTH TLS".into()),
        ..sin.clone()
    };
    let back: ConnectionDegraded =
        serde_json::from_value(serde_json::to_value(&con).unwrap()).unwrap();
    assert_eq!(back, con);
}

#[test]
fn session_undo_roundtrip() {
    use norte_proto::methods::{PolicyUndoSessionParams, PolicyUndoSessionResult};
    let p: PolicyUndoSessionParams = serde_json::from_str(r#"{"session":"claude"}"#).unwrap();
    assert_eq!(p.session, "claude");
    assert_eq!(
        serde_json::to_string(&p).unwrap(),
        r#"{"session":"claude"}"#
    );
    let r: PolicyUndoSessionResult = serde_json::from_str(r#"{"task_id":9}"#).unwrap();
    assert_eq!(r.task_id.get(), 9);
}

#[test]
fn plugin_types_roundtrip() {
    use norte_proto::methods::{
        PluginInfo, PluginListParams, PluginListResult, PluginLoadError, PluginSetApprovalParams,
        PluginSetEnabledParams,
    };
    // plugin.list no lleva params (objeto vacío, patrón de task.list).
    assert_eq!(serde_json::to_string(&PluginListParams {}).unwrap(), "{}");
    let res = PluginListResult {
        plugins: vec![PluginInfo {
            id: "org.norte.demo".into(),
            name: "Demo".into(),
            publisher: "norte".into(),
            version: "0.1.0".into(),
            category: "previewer".into(),
            capabilities: vec!["fs-read".into()],
            approved: false,
            enabled: true,
            description: None,
            commands: vec![],
            columns: vec![],
        }],
        errors: vec![PluginLoadError {
            dir: "/plugins/broken".into(),
            reason: "manifiesto inválido".into(),
        }],
    };
    let back: PluginListResult =
        serde_json::from_str(&serde_json::to_string(&res).unwrap()).unwrap();
    assert_eq!(back, res);
    let ap: PluginSetApprovalParams =
        serde_json::from_str(r#"{"id":"org.norte.demo","approved":true}"#).unwrap();
    assert!(ap.approved);
    assert_eq!(ap.id, "org.norte.demo");
    let en: PluginSetEnabledParams =
        serde_json::from_str(r#"{"id":"org.norte.demo","enabled":false}"#).unwrap();
    assert!(!en.enabled);
}

/// (P1, 0.26.0) Tolerancia N-1: un peer viejo que emite `PluginInfo` SIN
/// `description`/`commands` (shape de 0.25.x) debe seguir deserializando
/// aquí — ambos campos caen a su default (`None`/`vec![]`), nunca un error.
#[test]
fn plugin_info_old_shape_tolerance() {
    use norte_proto::methods::PluginInfo;
    let old_shape = r#"{
        "id": "org.norte.demo",
        "name": "Demo Previewer",
        "publisher": "norte",
        "version": "0.1.0",
        "category": "previewer",
        "capabilities": ["fs-read"],
        "approved": true,
        "enabled": true
    }"#;
    let info: PluginInfo = serde_json::from_str(old_shape).expect("shape 0.25.x tolerado");
    assert_eq!(info.description, None);
    assert!(info.commands.is_empty());
}

/// (P1, 0.26.0) Estabilidad de bytes hacia atrás: cuando `description` es
/// `None` (el default, y lo que un plugin sin manifiesto-description
/// produce hoy), la clave NO sale al wire — un peer N-1 que solo conoce el
/// shape de 0.25.x ve exactamente lo de antes salvo por el nuevo
/// `commands` (aditivo, siempre presente aunque vacío).
#[test]
fn plugin_info_none_description_omitted_on_wire() {
    use norte_proto::methods::PluginInfo;
    let info = PluginInfo {
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
    };
    let wire = serde_json::to_string(&info).unwrap();
    assert!(
        !wire.contains("description"),
        "description:None no debe serializarse: {wire}"
    );
    assert!(
        wire.contains(r#""commands":[]"#),
        "commands es aditivo pero SIEMPRE presente (sin skip_if vacío): {wire}"
    );
    assert!(
        wire.contains(r#""columns":[]"#),
        "columns (0.28.0, G3c) es aditivo pero SIEMPRE presente (sin skip_if vacío): {wire}"
    );
}

/// (G3c, 0.28.0) Tolerancia N-1: un peer que emite `PluginInfo` en el shape
/// 0.27.x (sin `columns`) debe seguir deserializando aquí — cae a su
/// default (`vec![]`), mismo criterio que `plugin_info_old_shape_tolerance`
/// para `description`/`commands` en 0.26.0.
#[test]
fn plugin_info_pre_028_shape_tolerance() {
    use norte_proto::methods::PluginInfo;
    let shape_027 = r#"{
        "id": "org.norte.demo",
        "name": "Demo Previewer",
        "publisher": "norte",
        "version": "0.1.0",
        "category": "previewer",
        "capabilities": ["fs-read"],
        "approved": true,
        "enabled": true,
        "commands": []
    }"#;
    let info: PluginInfo = serde_json::from_str(shape_027).expect("shape 0.27.x tolerado");
    assert!(info.columns.is_empty());
}

#[test]
fn plugin_run_command_roundtrip() {
    use norte_proto::methods::{PluginRunCommandParams, PluginRunCommandResult};
    // Con `arg` explícito: round-trip exacto.
    let p = PluginRunCommandParams {
        id: "org.norte.demo".into(),
        command: "greet".into(),
        arg: "world".into(),
    };
    let back: PluginRunCommandParams =
        serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
    assert_eq!(back, p);
    // `arg` ausente → `""` (el default), y sin skip_serializing_if SIEMPRE
    // sale en el wire: un cliente que lo omite obtiene `arg:""` al reserializar.
    let sin_arg: PluginRunCommandParams =
        serde_json::from_str(r#"{"id":"org.norte.demo","command":"greet"}"#).unwrap();
    assert_eq!(sin_arg.arg, "");
    assert_eq!(
        serde_json::to_string(&sin_arg).unwrap(),
        r#"{"id":"org.norte.demo","command":"greet","arg":""}"#
    );
    let r: PluginRunCommandResult = serde_json::from_str(r#"{"output":"hello, world"}"#).unwrap();
    assert_eq!(r.output, "hello, world");
}

#[test]
fn plugin_preview_roundtrip() {
    use norte_proto::methods::{PluginPreview, PluginPreviewParams, PluginPreviewResult};
    // Params con un VPath: round-trip exacto.
    let p = PluginPreviewParams {
        path: vpath("file:///a.txt"),
    };
    let back: PluginPreviewParams =
        serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
    assert_eq!(back, p);
    // Result POBLADO (Some): round-trip exacto, flatten al nivel raíz.
    let full = PluginPreviewResult {
        preview: Some(PluginPreview {
            plugin_id: "org.norte.md".into(),
            plugin_name: "Markdown Preview".into(),
            output: "<h1>Título</h1>".into(),
            lossy: false,
        }),
    };
    let full_json = serde_json::to_string(&full).unwrap();
    assert!(
        full_json.contains("\"plugin_id\":\"org.norte.md\""),
        "flatten: {full_json}"
    );
    let back_full: PluginPreviewResult = serde_json::from_str(&full_json).unwrap();
    assert_eq!(back_full, full);
    // 0.29.0 (#101): `lossy` es aditivo — un wire N-1 (0.28.x) SIN el campo
    // deserializa a `false` (sin aviso, dirección segura).
    let n1: PluginPreviewResult = serde_json::from_str(
        r#"{"plugin_id":"org.norte.md","plugin_name":"Markdown Preview","output":"x"}"#,
    )
    .expect("shape 0.28.x tolerado");
    assert!(!n1.preview.unwrap().lossy, "lossy ausente = false (N-1)");
    // Result VACÍO: `{}` deserializa a None y reserializa a `{}` (ningún
    // previewer aplica — el frontend cae a la vista cruda).
    let none: PluginPreviewResult = serde_json::from_str("{}").unwrap();
    assert_eq!(none.preview, None);
    assert_eq!(serde_json::to_string(&none).unwrap(), "{}");
    // Estado PARCIAL: el tipo Rust lo hace INCONSTRUIBLE (preview es un
    // `Option<PluginPreview>` de campos requeridos); en el wire un objeto con
    // solo algunos campos colapsa a `None` (sin preview, seguro) — jamás un
    // `plugin_id` sin `output`.
    let parcial: PluginPreviewResult =
        serde_json::from_str(r#"{"plugin_id":"x"}"#).expect("parcial deserializa");
    assert_eq!(parcial.preview, None, "un preview parcial cae a None");
}

/// `plugin.preview_styled` (0.27.0, G3, ADR 0037): mismo patrón
/// all-or-nothing que `plugin_preview_roundtrip` de arriba, con `lines` de
/// spans en vez de un `output` plano.
#[test]
fn plugin_preview_styled_roundtrip() {
    use norte_proto::methods::{
        PluginPreviewStyled, PluginPreviewStyledParams, PluginPreviewStyledResult, SpanWire,
    };
    let p = PluginPreviewStyledParams {
        path: vpath("file:///a.rs"),
    };
    let back: PluginPreviewStyledParams =
        serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
    assert_eq!(back, p);
    // Result POBLADO: round-trip exacto, flatten al nivel raíz (como
    // `plugin_preview_result`).
    let full = PluginPreviewStyledResult {
        preview: Some(PluginPreviewStyled {
            plugin_id: "org.norte.demo".into(),
            plugin_name: "Demo Previewer".into(),
            lines: vec![vec![SpanWire {
                text: "fn".into(),
                role: Some("match".into()),
                fg: None,
            }]],
            lossy: false,
        }),
    };
    let full_json = serde_json::to_string(&full).unwrap();
    assert!(
        full_json.contains("\"plugin_id\":\"org.norte.demo\""),
        "flatten: {full_json}"
    );
    let back_full: PluginPreviewStyledResult = serde_json::from_str(&full_json).unwrap();
    assert_eq!(back_full, full);
    // Result VACÍO: `{}` deserializa a None y reserializa a `{}`.
    let none: PluginPreviewStyledResult = serde_json::from_str("{}").unwrap();
    assert_eq!(none.preview, None);
    assert_eq!(serde_json::to_string(&none).unwrap(), "{}");
    // Estado PARCIAL: inconstruible en Rust (los tres campos van juntos en
    // `PluginPreviewStyled`); un objeto parcial del wire colapsa a `None`.
    let parcial: PluginPreviewStyledResult =
        serde_json::from_str(r#"{"plugin_id":"x"}"#).expect("parcial deserializa");
    assert_eq!(
        parcial.preview, None,
        "un preview con estilo parcial cae a None"
    );
}

/// `SpanWire`/`DecorationWire` (0.27.0, G3, ADR 0037): `role`/`fg`/`badge`
/// son `Option` independientes con `skip_serializing_if` — cuando faltan,
/// NO salen al wire (payload mínimo, mismo trato que `description` en
/// `PluginInfo`), y un shape que solo trae `text`/vacío tolera su ausencia
/// al deserializar.
#[test]
fn span_wire_and_decoration_wire_optionals_are_independent_and_omitted() {
    use norte_proto::methods::{DecorationWire, SpanWire};
    let bare = SpanWire {
        text: "fn".into(),
        role: None,
        fg: None,
    };
    assert_eq!(
        serde_json::to_string(&bare).unwrap(),
        r#"{"text":"fn"}"#,
        "role/fg ausentes no salen al wire"
    );
    let only_role: SpanWire = serde_json::from_str(r#"{"text":"x","role":"error"}"#).unwrap();
    assert_eq!(only_role.role.as_deref(), Some("error"));
    assert_eq!(only_role.fg, None);
    let only_fg: SpanWire = serde_json::from_str(r#"{"text":"x","fg":[1,2,3]}"#).unwrap();
    assert_eq!(only_fg.fg, Some([1, 2, 3]));
    assert_eq!(only_fg.role, None);

    let empty_decoration = DecorationWire {
        badge: None,
        role: None,
    };
    assert_eq!(
        serde_json::to_string(&empty_decoration).unwrap(),
        "{}",
        "una decoración sin badge ni role serializa a objeto vacío, no null"
    );
    let back: DecorationWire = serde_json::from_str("{}").unwrap();
    assert_eq!(back, empty_decoration);
}

/// `plugin.decorate`/`plugin.column_values` (0.27.0, G3, ADR 0037): POSICIÓN
/// 1:1 con `paths`, jamás un mapa clave→valor — un elemento sin dato para
/// esa ruta sigue presente (no se omite), y el orden se preserva byte-exacto
/// incluyendo un nombre HOSTIL (no-UTF8).
#[test]
fn plugin_decorate_and_column_values_are_positional() {
    use norte_proto::methods::{
        DecorationWire, PluginColumnValuesParams, PluginColumnValuesResult, PluginDecorateParams,
        PluginDecorateResult, PluginDecorations,
    };
    let paths = vec![
        vpath("file:///repo/a.rs"),
        vpath("file:///repo/informe%FF%FE.dat"),
    ];
    let dp = PluginDecorateParams {
        paths: paths.clone(),
    };
    let back: PluginDecorateParams =
        serde_json::from_str(&serde_json::to_string(&dp).unwrap()).unwrap();
    assert_eq!(back, dp);

    let dr = PluginDecorateResult {
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
    };
    assert_eq!(
        dr.plugins[0].decorations.len(),
        paths.len(),
        "una decoración por ruta, sin omitir la que no tiene badge"
    );
    let back: PluginDecorateResult =
        serde_json::from_str(&serde_json::to_string(&dr).unwrap()).unwrap();
    assert_eq!(back, dr);

    let cvp = PluginColumnValuesParams {
        column_id: "git-status".into(),
        paths: paths.clone(),
    };
    // `Some("")` (celda real, cadena vacía) y `None` (la columna no aplica a
    // esa entrada) deben distinguirse en el wire — `values: Vec<Option
    // <String>>`, no `Vec<String>` (MAJOR de protocol-guardian aplicado).
    let cvr = PluginColumnValuesResult {
        values: vec![Some(String::new()), None],
    };
    assert_eq!(cvr.values.len(), cvp.paths.len());
    let wire = serde_json::to_string(&cvr).unwrap();
    assert_eq!(
        wire, r#"{"values":["",null]}"#,
        "celda vacía real (\"\") y celda ausente (null) son shapes DISTINTOS"
    );
    let back: PluginColumnValuesResult = serde_json::from_str(&wire).unwrap();
    assert_eq!(back, cvr);
}

#[test]
fn rpc_cancel_params_round_trip_num_y_str() {
    use norte_proto::methods::RpcCancelParams;
    use norte_proto::wire::RequestId;
    for id in [RequestId::Num(42), RequestId::Str("abc".into())] {
        let p = RpcCancelParams { id: id.clone() };
        let wire = serde_json::to_string(&p).expect("serializa");
        let back: RpcCancelParams = serde_json::from_str(&wire).expect("deserializa");
        assert_eq!(back.id, id);
    }
    assert_eq!(
        serde_json::to_value(RpcCancelParams {
            id: RequestId::Num(7)
        })
        .unwrap(),
        serde_json::json!({"id": 7}),
    );
}
