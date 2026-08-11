//! Tests deterministas de los tipos del protocolo (fase 4 de M0): serde
//! roundtrip, tolerancia a campos desconocidos (forward-compat N/N-1) y
//! semántica de cada tipo. El wire byte-exacto vive en `golden_types.rs`.

use norte_proto::{
    AttrValue, Capabilities, CapabilityFlags, ConflictKind, Entry, EntryKind, Error, TaskId,
    TaskKind, TaskProgress, TaskState, VPath,
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
        attrs: std::collections::BTreeMap::new(),
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
        attrs: std::collections::BTreeMap::new(),
        path: vpath("file:///dir"),
        kind: EntryKind::Dir,
        size: None,
        mtime_ms: None,
    };
    assert_eq!(roundtrip(&e), e);
}

/// (0.30.0, ADR 0039 §4) Una clave de atributo mal formada se DESCARTA al
/// decodificar — jamás es un error, igual que una celda malformada degrada a
/// `Unknown`: una clave mala cuesta esa clave, nunca la entrada ni la página.
/// El filtro vive en el TIPO, no en cada llamador, porque un id acaba siendo
/// un id de configuración y una clave de lookup aguas abajo.
#[test]
fn entry_descarta_claves_de_atributo_mal_formadas() {
    let json = r#"{
        "path": "file:///a",
        "kind": "file",
        "attrs": {
            "posix.mode": {"uint": 33188},
            "s3.storage_class": {"text": "STANDARD_IA"},
            "MODE": {"uint": 1},
            "../etc/passwd": {"text": "no"},
            "mode": {"uint": 2},
            "": {"uint": 3},
            "posix.": {"uint": 4},
            "posix mode": {"uint": 5}
        }
    }"#;
    let e: Entry = serde_json::from_str(json).expect("una clave mala NO rompe la entrada");

    let claves: Vec<&str> = e.attrs.keys().map(String::as_str).collect();
    assert_eq!(
        claves,
        vec!["posix.mode", "s3.storage_class"],
        "solo sobreviven los ids bien formados y namespaced"
    );
    assert_eq!(e.attrs["posix.mode"], AttrValue::Uint(33188));
}

/// (0.30.0, ADR 0039 §5) El mapa está ACOTADO al decodificar: un cliente puede
/// pedir a lo sumo `ATTRS_MAX_REQUEST` ids, así que un mapa mayor es un peer
/// con bugs o hostil. Se quedan las claves MENORES en orden de bytes, así que
/// el CONJUNTO de ids que sobrevive no depende del orden en que el peer
/// serializó — los objetos JSON no están ordenados (RFC 8259 §4). Ojo: aquí
/// las claves son DISTINTAS; el caso de una clave repetida (last-wins) lo
/// cubre `entry_clave_de_atributo_repetida_es_last_wins`.
#[test]
fn entry_acota_el_mapa_de_atributos_de_forma_determinista() {
    use norte_proto::attrs::ATTRS_MAX_REQUEST;

    let ids: Vec<String> = (0..40).map(|i| format!("test.attr_{i:02}")).collect();
    let celdas = |orden: &dyn Fn(&mut Vec<&String>)| {
        let mut claves: Vec<&String> = ids.iter().collect();
        orden(&mut claves);
        let cuerpo: Vec<String> = claves
            .iter()
            .map(|id| format!("\"{id}\":{{\"uint\":1}}"))
            .collect();
        let json = format!(
            r#"{{"path":"file:///a","kind":"file","attrs":{{{}}}}}"#,
            cuerpo.join(",")
        );
        let e: Entry = serde_json::from_str(&json).expect("un mapa gordo NO rompe la entrada");
        e.attrs.into_keys().collect::<Vec<String>>()
    };

    let esperado: Vec<String> = {
        let mut v = ids.clone();
        v.sort();
        v.truncate(ATTRS_MAX_REQUEST);
        v
    };
    assert_eq!(esperado.len(), ATTRS_MAX_REQUEST);

    let ascendente = celdas(&|c| c.sort());
    let descendente = celdas(&|c| c.sort_by(|a, b| b.cmp(a)));
    assert_eq!(ascendente, esperado, "se quedan las menores en bytes");
    assert_eq!(
        descendente, ascendente,
        "el resultado NO depende del orden de claves del wire"
    );
}

/// (0.30.0, rust-review MAJOR 1) Una clave REPETIDA en el mismo objeto se
/// resuelve last-wins — como en cualquier parser JSON, y como hacía el
/// `BTreeMap` derivado al que este deserializador sustituye. El test de
/// determinismo permuta claves DISTINTAS y no ve este caso: sin el atajo de
/// "la clave ya está en el mapa", el valor conservado dependía de si el mapa
/// estaba lleno cuando llegó el duplicado.
#[test]
fn entry_clave_de_atributo_repetida_es_last_wins() {
    use norte_proto::attrs::ATTRS_MAX_REQUEST;

    fn decodifica(pares: &[(String, u64)]) -> Entry {
        let cuerpo: Vec<String> = pares
            .iter()
            .map(|(k, v)| format!("\"{k}\":{{\"uint\":{v}}}"))
            .collect();
        let json = format!(
            r#"{{"path":"file:///a","kind":"file","attrs":{{{}}}}}"#,
            cuerpo.join(",")
        );
        serde_json::from_str(&json).expect("una clave repetida NO rompe la entrada")
    }

    // Por DEBAJO del tope: el duplicado gana, esté donde esté.
    let dup_al_final = decodifica(&[
        ("a.k00".to_owned(), 1),
        ("b.k00".to_owned(), 9),
        ("a.k00".to_owned(), 2),
    ]);
    let dup_al_principio = decodifica(&[
        ("a.k00".to_owned(), 1),
        ("a.k00".to_owned(), 2),
        ("b.k00".to_owned(), 9),
    ]);
    assert_eq!(dup_al_final.attrs["a.k00"], AttrValue::Uint(2));
    assert_eq!(dup_al_final.attrs, dup_al_principio.attrs);

    // EN el tope, repitiendo la clave MAYOR (la que la poda expulsaría): el
    // mapa ya está lleno cuando llega el duplicado en un orden y no en el
    // otro, y aun así el resultado debe ser el mismo.
    let llenas: Vec<(String, u64)> = (0..ATTRS_MAX_REQUEST)
        .map(|i| (format!("a.k{i:02}"), 1))
        .collect();
    let mayor = format!("a.k{:02}", ATTRS_MAX_REQUEST - 1);

    let mut dup_despues = llenas.clone();
    dup_despues.push((mayor.clone(), 2));

    let mut dup_antes = vec![(mayor.clone(), 1), (mayor.clone(), 2)];
    dup_antes.extend(llenas.iter().filter(|(k, _)| *k != mayor).cloned());

    let a = decodifica(&dup_despues);
    let b = decodifica(&dup_antes);
    assert_eq!(a.attrs.len(), ATTRS_MAX_REQUEST);
    assert_eq!(
        a.attrs[&mayor],
        AttrValue::Uint(2),
        "last-wins también con el mapa lleno"
    );
    assert_eq!(
        a.attrs, b.attrs,
        "mismos miembros en distinto orden → mismo resultado, valores incluidos"
    );
}

/// (0.30.0, rust-review MAJOR 2) El filtro es de UNA dirección: `attrs` es un
/// campo público sin constructor y la serialización NO filtra, así que un
/// `Entry` construido en proceso con un id inválido o por encima del tope
/// EMITE lo que lleva y vuelve DISTINTO. Se fija aquí para que nadie asuma
/// round-trip identidad: el bug del productor tiene que seguir siendo visible
/// en la frontera que lo valida (bloque 2), no lavado por el serializador.
#[test]
fn entry_construida_en_proceso_no_esta_filtrada_y_no_hace_roundtrip() {
    use norte_proto::attrs::ATTRS_MAX_REQUEST;

    let con_id_invalido = Entry {
        attrs: std::collections::BTreeMap::from([("MODE".to_owned(), AttrValue::Uint(1))]),
        ..sample_entry()
    };
    let wire = serde_json::to_string(&con_id_invalido).expect("serializable");
    assert!(
        wire.contains("MODE"),
        "la serialización NO filtra: el bug del productor viaja: {wire}"
    );
    assert_ne!(
        roundtrip(&con_id_invalido),
        con_id_invalido,
        "y al volver la clave inválida ya no está"
    );

    let gorda = Entry {
        attrs: (0..ATTRS_MAX_REQUEST + 5)
            .map(|i| (format!("test.attr_{i:02}"), AttrValue::Uint(i as u64)))
            .collect(),
        ..sample_entry()
    };
    assert_eq!(
        roundtrip(&gorda).attrs.len(),
        ATTRS_MAX_REQUEST,
        "por encima del tope se emite entero pero se decodifica acotado"
    );
}

/// Guardia de regresión del filtro: una entrada normal (ids válidos, por
/// debajo del tope) hace round-trip EXACTO, atributos incluidos.
#[test]
fn entry_con_atributos_validos_hace_roundtrip_exacto() {
    let e = Entry {
        attrs: std::collections::BTreeMap::from([
            ("posix.mode".to_owned(), AttrValue::Uint(0o100_644)),
            ("sftp.owner".to_owned(), AttrValue::Bytes(vec![0xFF, 0xFE])),
            (
                "s3.storage_class".to_owned(),
                AttrValue::Text("STANDARD_IA".to_owned()),
            ),
        ]),
        ..sample_entry()
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
        (TaskKind::RenameBatch, "\"rename_batch\""),
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
fn rename_collision_unknown_kind_degrades_nested() {
    // Mismo criterio que `conflict_unknown_subtype_degrades_nested` (ADR 0005),
    // y aquí lo que está en juego es el PLAN entero: un veredicto de un daemon
    // 0.37 (que `version_compatible` acepta frente a un cliente 0.36) no puede
    // dejar al humano sin plan que revisar — degrada esa línea, no el documento.
    use norte_proto::methods::{FsRenameBatchPlanResult, RenameCollisionKind};
    // El hash es uno VÁLIDO (64 hex minúscula): con `"00"` este test pasaba
    // por el camino equivocado y, de paso, pineaba que cualquier cadena es un
    // plan hash. Lo que aquí se demuestra es el fallback del veredicto, nada
    // más.
    let json = format!(
        r#"{{"steps":[],"collisions":[{{"pair_index":3,"name":"a","kind":"veredicto_del_futuro"}}],
            "executable":false,"plan_hash":"{}"}}"#,
        "ab".repeat(32)
    );
    let r: FsRenameBatchPlanResult =
        serde_json::from_str(&json).expect("un veredicto desconocido NO revienta el plan");
    assert_eq!(r.collisions.len(), 1);
    assert_eq!(r.collisions[0].kind, RenameCollisionKind::Unknown);
    assert_eq!(r.collisions[0].name.as_bytes(), b"a");
    // Y la fila SIGUE siendo señalable: `pair_index` no depende de `kind`, que
    // es justo lo que hace útil al fallback en vez de decorativo.
    assert_eq!(r.collisions[0].pair_index, 3);
}

/// (0.36.0) Tolerancia N-1 sobre un tipo que SÍ estaba vivo: un daemon 0.35
/// emite `PolicyUndoReportResult` sin `batch_stuck` ni `compensations_lost`, y
/// ese informe tiene que seguir deserializando aquí. La ausencia significa
/// exactamente lo que parece — ese daemon no sabía deshacer lotes, así que no
/// pudo dejar ninguno a medias.
///
/// Es el `serde(default)` de `compensations_lost` lo que se está demostrando:
/// las goldens llevan la clave SIEMPRE (viaja incluso en cero), así que sin
/// este test se podría borrar el atributo y la suite seguiría verde.
#[test]
fn policy_undo_report_result_shape_0_35_tolerada() {
    use norte_proto::methods::{FsRenameBatchReportResult, PolicyUndoReportResult};
    let old_shape = r#"{
        "undone": 4,
        "skipped_irreversible": 1,
        "skipped_created_no_trash": 0
    }"#;
    let r: PolicyUndoReportResult = serde_json::from_str(old_shape).expect("shape 0.35.x tolerada");
    assert_eq!(r.undone, 4);
    assert!(r.batch_stuck.is_none());
    assert_eq!(r.compensations_lost, 0);

    // Y el informe del LOTE, nacido en este mismo bump, aguanta lo mismo: sus
    // tres opcionales ausentes son «no pasó nada de eso», no un error de parse.
    let minimo: FsRenameBatchReportResult =
        serde_json::from_str(r#"{"applied":3,"rolled_back":0}"#).expect("mínimo tolerado");
    assert!(minimo.stuck.is_none());
    assert!(minimo.uncertain.is_none());
    assert!(minimo.failed_pair.is_none());
    assert_eq!(minimo.compensations_lost, 0);
}

/// El `plan_hash` se valida en la DESERIALIZACIÓN, así que una forma
/// equivocada muere en el borde (`-32602` para el daemon) y jamás llega al
/// comparador, que es donde se convertiría en un `PlanStale` mentiroso.
#[test]
fn plan_hash_malformado_muere_en_el_wire() {
    use norte_proto::methods::{FsRenameBatchParams, PlanHash};
    let params = |hash: &str| format!(r#"{{"dir":"file:///d","pairs":[],"plan_hash":"{hash}"}}"#);
    // Mayúsculas: MISMO hash, otra escritura — se rechaza para que dos formas
    // del mismo valor no comparen distinto según quién lo escribió.
    for malo in [
        "00",
        &"AB".repeat(32),
        &"ab".repeat(33),
        &format!("{}g", "a".repeat(63)),
    ] {
        assert!(
            serde_json::from_str::<FsRenameBatchParams>(&params(malo)).is_err(),
            "{malo} no es un plan hash"
        );
    }
    let bueno = "ab".repeat(32);
    let ok: FsRenameBatchParams = serde_json::from_str(&params(&bueno)).expect("64 hex minúscula");
    assert_eq!(ok.plan_hash, PlanHash::parse(&bueno).expect("hash"));
    // Round-trip: lo que sale es exactamente lo que entró.
    assert_eq!(ok.plan_hash.as_str(), bueno);
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
        paths_total: 9,
        ttl_ms: 30_000,
    };
    let back: PolicyApprovalRequired =
        serde_json::from_str(&serde_json::to_string(&ar).unwrap()).unwrap();
    assert_eq!(back, ar);
    // Tolerancia N-1 (0.36.0): un server 0.35 no manda `paths_total`, y su
    // ausencia cae a 0 = DESCONOCIDO, que es lo que ese server podía decir.
    let viejo: PolicyApprovalRequired = serde_json::from_str(
        r#"{"approval_id":7,"op":"delete","paths":["file:///work/x"],"ttl_ms":30000}"#,
    )
    .expect("shape 0.35.x tolerada");
    assert_eq!(viejo.paths_total, 0);
    let dec: PolicyDecideParams =
        serde_json::from_str(r#"{"approval_id":7,"approve":true}"#).unwrap();
    assert!(dec.approve);
}

#[test]
fn version_ventana_actual() {
    use norte_proto::PROTOCOL_VERSION;
    use norte_proto::methods::version_compatible;
    // 0.40.0 (sync.plan, ADR 0049): acepta 0.40.x (N) y 0.39.x (N-1),
    // rechaza 0.38.x (N-2) — la ventana se desplaza con el bump, no se
    // ensancha, y que el bump sea ADITIVO no la ensancha tampoco.
    assert!(version_compatible(PROTOCOL_VERSION, "0.40.9"), "N");
    assert!(version_compatible(PROTOCOL_VERSION, "0.39.0"), "N-1");
    assert!(
        !version_compatible(PROTOCOL_VERSION, "0.38.9"),
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
            has_help: false,
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
        has_help: false,
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
        plugin_id: None,
    };
    // Ausente NO se emite: la petición de un cliente que no nombra plugin es
    // byte a byte la de 0.34 (#120).
    assert!(
        !serde_json::to_string(&cvp).unwrap().contains("plugin_id"),
        "plugin_id ausente no debe aparecer en el wire"
    );
    let cvp_scoped = PluginColumnValuesParams {
        plugin_id: Some("org.norte.git".into()),
        ..cvp.clone()
    };
    let back: PluginColumnValuesParams =
        serde_json::from_str(&serde_json::to_string(&cvp_scoped).unwrap()).unwrap();
    assert_eq!(back.plugin_id.as_deref(), Some("org.norte.git"));
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

/// (0.30.0, ADR 0039) Los CUATRO campos nuevos —repartidos en TRES superficies:
/// catálogo, petición ×2 y entrada— son aditivos: un wire N-1 (0.29.x) SIN
/// ellos deserializa, y un valor vacío NO se emite. (`Entry.attrs`, el cuarto,
/// lo cubre `entry_con_atributos_validos_hace_roundtrip_exacto` y la golden
/// `attrs_vacios_se_omiten`.)
#[test]
fn attrs_son_aditivos_en_ambas_direcciones() {
    use norte_proto::methods::{FsCapabilitiesResult, FsListParams, FsStatParams};

    let n1 = r#"{"path":"file:///home","limit":null,"cursor":null}"#;
    let params: FsListParams = serde_json::from_str(n1).expect("wire N-1 válido");
    assert!(params.attrs.is_empty(), "ausente = ninguno pedido");

    let wire = serde_json::to_string(&params).unwrap();
    assert!(
        !wire.contains("attrs"),
        "vacío no se emite (byte-idéntico a 0.29): {wire}"
    );

    let stat: FsStatParams =
        serde_json::from_str(r#"{"path":"file:///home"}"#).expect("wire N-1 válido");
    assert!(stat.attrs.is_empty());
    assert!(!serde_json::to_string(&stat).unwrap().contains("attrs"));

    let caps_n1 = r#"{"capabilities":{"flags":"RENAME_ATOMIC","max_path":null}}"#;
    let caps: FsCapabilitiesResult = serde_json::from_str(caps_n1).expect("wire N-1 válido");
    assert!(caps.attrs.is_empty());
    assert!(!serde_json::to_string(&caps).unwrap().contains("attrs"));
}

/// (0.30.0, ADR 0039) La asimetría es DELIBERADA y ejecutable: el catálogo
/// (dato RECIBIDO) filtra al decodificar; una petición (dato ENVIADO) no —
/// un id mal formado sobrevive para que el daemon lo responda `-32602` en el
/// bloque 2, en vez de convertirse en "no pidió nada".
#[test]
fn peticion_no_filtra_pero_el_catalogo_si() {
    use norte_proto::methods::{FsCapabilitiesResult, FsListParams, FsStatParams};

    let hostil = r#"{"path":"file:///home","attrs":["MODE","../etc/passwd"]}"#;
    let list: FsListParams = serde_json::from_str(hostil).expect("la petición decodifica tal cual");
    assert_eq!(list.attrs, ["MODE", "../etc/passwd"], "nada se descarta");
    let stat: FsStatParams = serde_json::from_str(hostil).expect("igual en fs.stat");
    assert_eq!(stat.attrs, ["MODE", "../etc/passwd"]);

    let catalogo = r#"{
        "capabilities": {"flags":"RENAME_ATOMIC","max_path":null},
        "attrs": [
            {"id":"MODE","label":"Mode","type":"uint","hint":"mode"},
            {"id":"posix.mode","label":"Mode","type":"uint","hint":"mode"}
        ]
    }"#;
    let caps: FsCapabilitiesResult =
        serde_json::from_str(catalogo).expect("un catálogo hostil no rompe la respuesta");
    let ids: Vec<&str> = caps.attrs.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["posix.mode"], "el id mal formado no se puede pedir");
}

/// (0.30.0) Ids de EJEMPLO —plausibles, no un vocabulario: ADR 0039 §4 se niega
/// explícitamente a un registro central y no registra ninguno de estos, que un
/// provider puede o no publicar— con la forma bien construida, frente a las
/// formas hostiles que NO lo están. Lo que se fija es la GRAMÁTICA, y que el
/// gate vive en el tipo y no en cada llamador.
#[test]
fn ids_de_ejemplo_bien_formados() {
    use norte_proto::attrs::is_valid_attr_id;

    for id in [
        "posix.mode",
        "posix.uid",
        "posix.gid",
        "posix.nlink",
        "posix.ctime_ms",
        "win.attributes",
        "sftp.owner",
        "sftp.group",
        "s3.storage_class",
        "s3.etag",
        "s3.content_type",
        "archive.method",
        "archive.packed_size",
        "archive.crc32",
    ] {
        assert!(is_valid_attr_id(id), "{id} es un ejemplo bien formado");
    }
    for hostil in [
        "../etc/passwd",
        "posix.mode\u{202E}",
        "POSIX.MODE",
        "",
        // Segmento que no empieza por letra (0.30.0): forma de argv y de float.
        "-x.y",
        "0.0",
    ] {
        assert!(!is_valid_attr_id(hostil), "{hostil:?} debe rechazarse");
    }
}

// ---------- fs.compare (0.39.0) ----------

/// Un [`CompareRow`] mínimo sobre el que cada test cambia solo lo suyo.
fn compare_row(
    verdict: norte_proto::methods::CompareVerdict,
    left: Option<Entry>,
    right: Option<Entry>,
) -> norte_proto::methods::CompareRow {
    use norte_proto::methods::{CompareConfidence, CompareCriterion, CompareRow};
    CompareRow {
        id: 1,
        left,
        right,
        verdict,
        criterion: CompareCriterion::Presence,
        confidence: CompareConfidence::Certain,
        newer: None,
        reason: None,
        side: None,
    }
}

fn compare_entry(wire: &str) -> Entry {
    Entry {
        path: vpath(wire),
        kind: EntryKind::File,
        size: Some(1),
        mtime_ms: Some(0),
        attrs: std::collections::BTreeMap::new(),
    }
}

/// Un daemon N+1 que añade un criterio no puede romper a un frontend N-1: el
/// token desconocido cae en la variante forward-compat, no da error.
#[test]
fn unknown_enum_tokens_degrade_and_do_not_error() {
    use norte_proto::methods::{CompareCriterion, CompareReason, CompareVerdict};
    let v: CompareVerdict = serde_json::from_str("\"teleported\"").expect("degrades");
    assert_eq!(v, CompareVerdict::Unknown);
    let c: CompareCriterion = serde_json::from_str("\"vibes\"").expect("degrades");
    assert_eq!(c, CompareCriterion::Unknown);
    let r: CompareReason = serde_json::from_str("\"gremlins\"").expect("degrades");
    assert_eq!(r, CompareReason::Unknown);
    let s: norte_proto::methods::Side = serde_json::from_str("\"middle\"").expect("degrades");
    assert_eq!(s, norte_proto::methods::Side::Unknown);
}

/// `Unknown` en CONFIDENCE es un VALOR — «el provider no puede decirlo» —, así
/// que el fallback forward-compat de ese enum tuvo que llamarse de otra manera.
/// Perder la distinción convertiría una respuesta honesta en un desajuste de
/// protocolo.
#[test]
fn confidence_unknown_is_a_value_not_the_fallback() {
    use norte_proto::methods::CompareConfidence;
    let known: CompareConfidence = serde_json::from_str("\"unknown\"").expect("a real value");
    assert_eq!(known, CompareConfidence::Unknown);
    let newer: CompareConfidence = serde_json::from_str("\"quantum\"").expect("degrades");
    assert_eq!(newer, CompareConfidence::Unrecognised);
    assert_ne!(known, newer);
}

/// La invariante que el wire no sabe expresar: el veredicto determina qué
/// lados están presentes. Una fila que dice `OnlyLeft` llevando entrada
/// derecha es un bug de quien la produjo, y aquí es donde se caza.
#[test]
fn verdict_determines_which_sides_are_present() {
    use norte_proto::methods::CompareVerdict;
    let a = || compare_entry("file:///a");
    let b = || compare_entry("file:///b");
    assert!(compare_row(CompareVerdict::OnlyLeft, Some(a()), None).sides_are_consistent());
    assert!(!compare_row(CompareVerdict::OnlyLeft, Some(a()), Some(b())).sides_are_consistent());
    assert!(compare_row(CompareVerdict::Same, Some(a()), Some(b())).sides_are_consistent());
    assert!(!compare_row(CompareVerdict::Same, Some(a()), None).sides_are_consistent());
    // El resto del vocabulario, por simetría con el de arriba.
    assert!(compare_row(CompareVerdict::OnlyRight, None, Some(b())).sides_are_consistent());
    assert!(!compare_row(CompareVerdict::OnlyRight, Some(a()), None).sides_are_consistent());
    assert!(compare_row(CompareVerdict::Different, Some(a()), Some(b())).sides_are_consistent());
    assert!(!compare_row(CompareVerdict::Different, None, None).sides_are_consistent());
    assert!(compare_row(CompareVerdict::TypeMismatch, Some(a()), Some(b())).sides_are_consistent());
    assert!(!compare_row(CompareVerdict::TypeMismatch, Some(a()), None).sides_are_consistent());
}

/// Los TRES veredictos sin regla de lados —los dos que describen un problema y
/// el fallback— no pueden fabricar una: `Ambiguous` nombra una colisión de UN
/// lado, un `Error` de listado puede no tener entrada que enseñar, y de un
/// veredicto que este cliente no conoce no se sabe nada. Afirmar lo contrario
/// haría que un cliente N-1 desconfiara de filas legítimas de un daemon N+1.
#[test]
fn problem_verdicts_have_no_side_rule_to_break() {
    use norte_proto::methods::CompareVerdict;
    let a = || compare_entry("file:///a");
    for verdict in [
        CompareVerdict::Ambiguous,
        CompareVerdict::Error,
        CompareVerdict::Unknown,
    ] {
        assert!(compare_row(verdict, None, None).sides_are_consistent());
        assert!(compare_row(verdict, Some(a()), None).sides_are_consistent());
        assert!(compare_row(verdict, Some(a()), Some(a())).sides_are_consistent());
    }
}

/// `reason` responde «por qué» exactamente para los dos veredictos que tienen
/// un porqué. En cualquier otro sitio es ruido que un cliente tendría que
/// adivinar.
#[test]
fn reason_belongs_to_ambiguous_and_error_only() {
    use norte_proto::methods::{CompareReason, CompareVerdict};
    for (verdict, reason, ok) in [
        (
            CompareVerdict::Ambiguous,
            Some(CompareReason::CaseFold),
            true,
        ),
        (CompareVerdict::Error, Some(CompareReason::Unreadable), true),
        (CompareVerdict::Ambiguous, None, false),
        (CompareVerdict::Same, Some(CompareReason::CaseFold), false),
        // El fallback queda EXENTO: un veredicto de N+1 puede traer motivo, y
        // un cliente N-1 no puede saber si le corresponde.
        (CompareVerdict::Unknown, Some(CompareReason::CaseFold), true),
        (CompareVerdict::Unknown, None, true),
    ] {
        let mut row = compare_row(verdict, None, None);
        row.reason = reason;
        assert_eq!(row.reason_is_consistent(), ok, "{verdict:?} + {reason:?}");
    }
}

/// Round-trip de la fila entera y de los params, con lo ausente AUSENTE del
/// wire (no `null`): la fila viaja millones de veces por `compare.rows`.
#[test]
fn compare_row_roundtrip_y_omisiones() {
    use norte_proto::methods::{CompareRow, CompareVerdict, Side};
    let mut row = compare_row(
        CompareVerdict::OnlyLeft,
        Some(compare_entry("file:///a")),
        None,
    );
    assert_eq!(roundtrip(&row), row);
    let json = serde_json::to_string(&row).expect("json");
    for ausente in ["right", "newer", "reason", "side"] {
        assert!(!json.contains(ausente), "{ausente} no debe viajar: {json}");
    }
    row.newer = Some(Side::Right);
    assert!(serde_json::to_string(&row).expect("json").contains("right"));
    // Campos desconocidos de un peer N+1 no rompen la fila.
    let futuro = r#"{"id":9,"verdict":"same","criterion":"size","confidence":"certain",
                     "campo_del_futuro":true}"#;
    let row: CompareRow = serde_json::from_str(futuro).expect("tolerante");
    assert_eq!(row.id, 9);
    assert!(row.left.is_none() && row.right.is_none());
}

/// Los criterios: `size` y `mtime` puestos, `hash` NO, y un objeto PARCIAL
/// completa desde ese default en vez de fallar. Es lo que decide si una
/// comparación lee contenido, así que el default importa tanto como el tipo.
#[test]
fn compare_criteria_default_y_parcial() {
    use norte_proto::methods::{CompareCriteria, FsCompareParams};
    let d = CompareCriteria::default();
    assert!(d.size && d.mtime && !d.hash);
    let parcial: CompareCriteria = serde_json::from_str(r#"{"hash":true}"#).expect("parcial");
    assert!(parcial.size && parcial.mtime && parcial.hash);

    let minimo = r#"{"left":"file:///a","right":"file:///b"}"#;
    let p: FsCompareParams = serde_json::from_str(minimo).expect("params mínimos");
    assert_eq!(p.criteria, CompareCriteria::default());
    assert_eq!(p.mtime_tolerance_ms, 2000, "la regla FAT, por defecto");
    assert!(p.max_depth.is_none() && !p.follow_symlinks);
    assert_eq!(roundtrip(&p), p);
}

// ---------- sync.plan (0.40.0) ----------

/// Un [`SyncStep`] mínimo sobre el que cada test cambia solo lo suyo.
fn sync_step(
    kind: norte_proto::methods::SyncStepKind,
    reversal: Option<norte_proto::methods::StepReversal>,
    reason: Option<norte_proto::methods::SyncReason>,
) -> norte_proto::methods::SyncStep {
    use norte_proto::methods::{CompareConfidence, CompareCriterion, RelPath, SyncStep};
    SyncStep {
        id: 1,
        kind,
        rel: RelPath::parse_wire("sub/b.txt").expect("rel"),
        size: Some(12),
        criterion: CompareCriterion::Size,
        confidence: CompareConfidence::Certain,
        reversal,
        reason,
    }
}

/// `reversal` es `None` si y SOLO si el paso es un `Skip`: un paso que no hace
/// nada no tiene nada que revertir, y uno que actúa debe decir cómo vuelve.
#[test]
fn a_skip_has_no_reversal_and_every_other_kind_has_one() {
    use norte_proto::methods::{StepReversal, SyncReason, SyncStepKind};
    assert!(
        sync_step(SyncStepKind::Skip, None, Some(SyncReason::AmbiguousSource))
            .shape_is_consistent()
    );
    assert!(
        !sync_step(
            SyncStepKind::Skip,
            Some(StepReversal::Delete),
            Some(SyncReason::AmbiguousSource)
        )
        .shape_is_consistent(),
        "a step that does nothing cannot claim a reversal"
    );
    assert!(sync_step(SyncStepKind::Copy, Some(StepReversal::Delete), None).shape_is_consistent());
    assert!(
        !sync_step(SyncStepKind::Copy, None, None).shape_is_consistent(),
        "an acting step must say how it comes back"
    );
}

/// `reason` viaja para EXACTAMENTE dos formas de paso: el `Skip` y el que
/// declara que no se puede deshacer. En cualquier otra es ruido.
#[test]
fn reason_is_present_for_exactly_skip_and_irreversible() {
    use norte_proto::methods::{StepReversal, SyncReason, SyncStepKind};
    assert!(
        sync_step(
            SyncStepKind::Skip,
            None,
            Some(SyncReason::UnknownConfidence)
        )
        .shape_is_consistent()
    );
    assert!(!sync_step(SyncStepKind::Skip, None, None).shape_is_consistent());
    assert!(
        sync_step(
            SyncStepKind::Overwrite,
            Some(StepReversal::Irreversible),
            Some(SyncReason::NoTrashOnTarget)
        )
        .shape_is_consistent()
    );
    assert!(
        !sync_step(
            SyncStepKind::Overwrite,
            Some(StepReversal::Irreversible),
            None
        )
        .shape_is_consistent(),
        "an irreversible step owes a reason"
    );
    assert!(
        !sync_step(
            SyncStepKind::Copy,
            Some(StepReversal::Delete),
            Some(SyncReason::Unreadable)
        )
        .shape_is_consistent(),
        "a reversible acting step has no reason to carry"
    );
}

/// Un daemon N+1 que añade una clase de paso no puede matar un lote de 256 en
/// un cliente N-1: el token desconocido degrada, como en la familia de
/// `compare.rows` (ADR 0048).
#[test]
fn an_unknown_step_kind_degrades_instead_of_killing_the_batch() {
    use norte_proto::methods::{SyncStep, SyncStepKind};
    let v = serde_json::json!({
        "id": 7, "kind": "teleport", "rel": "sub/a",
        "size": null, "criterion": "size", "confidence": "certain",
        "reversal": "delete", "reason": null
    });
    let s: SyncStep = serde_json::from_value(v).expect("degrades");
    assert_eq!(s.kind, SyncStepKind::Unknown);
    assert!(
        s.shape_is_consistent(),
        "un paso que este cliente no sabe juzgar no se declara inconsistente"
    );
}

/// Client→daemon: aceptar un modo desconocido por defecto es aceptar borrar
/// por defecto. Muere en el DESERIALIZADOR, que es más fuerte que cualquier
/// chequeo que un handler pueda olvidar.
#[test]
fn a_mode_this_daemon_does_not_know_is_refused_not_defaulted() {
    use norte_proto::methods::{OnUnknown, SyncMode};
    assert!(serde_json::from_value::<SyncMode>(serde_json::json!("obliterate")).is_err());
    assert!(serde_json::from_value::<OnUnknown>(serde_json::json!("maybe")).is_err());
}

/// Daemon→client: el vocabulario de bloqueos round-trippea entero.
#[test]
fn every_blocker_kind_round_trips() {
    use norte_proto::methods::SyncBlockerKind;
    for k in [
        SyncBlockerKind::AmbiguousDest,
        SyncBlockerKind::OverlapDetected,
        SyncBlockerKind::DestReadOnly,
        SyncBlockerKind::DirTooLarge,
    ] {
        let j = serde_json::to_value(k).expect("json");
        assert_eq!(
            serde_json::from_value::<SyncBlockerKind>(j).expect("back"),
            k
        );
    }
    // ...y el fallback de `#[serde(other)]` sigue ahí, como en compare.
    let futuro: SyncBlockerKind = serde_json::from_str("\"cosmic_ray\"").expect("degrades");
    assert_eq!(futuro, SyncBlockerKind::Unknown);
}

/// `rel` es RELATIVO y lo garantiza el TIPO: lo que se escaparía de la raíz no
/// llega a existir, y muere en el deserializador de cualquier peer — no en un
/// chequeo del daemon que se pueda olvidar. Un `VPath` no podía prometer esto
/// sin inventarse un scheme (ver el rustdoc de `RelPath`).
#[test]
fn a_rel_that_would_escape_its_root_dies_in_the_wire() {
    use norte_proto::methods::{RelPath, SyncStep};
    for hostil in ["..", "a/../b", "%2E%2E/etc", "/a", "a/", "a//b", ""] {
        let json = format!(
            r#"{{"id":1,"kind":"copy","rel":"{hostil}","criterion":"presence",
                 "confidence":"certain","reversal":"delete"}}"#
        );
        let paso = serde_json::from_str::<SyncStep>(&json);
        // La cadena vacía es la RAÍZ: legal, y es el único caso de la lista.
        assert_eq!(
            paso.is_ok(),
            hostil.is_empty(),
            "{hostil:?} no debe cruzar el wire como ruta relativa"
        );
    }
    assert!(RelPath::default().is_root());
    // Y los bytes vuelven intactos (regla dura 1), segmento a segmento.
    let r = RelPath::parse_wire("sub/informe%FF%FE.dat").expect("rel");
    assert_eq!(r.segments()[1].as_bytes(), b"informe\xff\xfe.dat");
    assert_eq!(r.to_wire(), "sub/informe%FF%FE.dat");
}

/// Lo ausente se OMITE del wire (ni `null` ni clave): un plan de medio millón
/// de pasos manda `sync.steps` miles de veces.
#[test]
fn sync_step_roundtrip_y_omisiones() {
    use norte_proto::methods::{StepReversal, SyncReason, SyncStep, SyncStepKind};
    let mut s = sync_step(SyncStepKind::Copy, Some(StepReversal::Delete), None);
    s.size = None;
    assert_eq!(roundtrip(&s), s);
    let json = serde_json::to_value(&s).expect("json");
    for ausente in ["size", "reason"] {
        assert!(
            json.get(ausente).is_none(),
            "{ausente} no debe viajar: {json}"
        );
    }
    // Campos desconocidos de un peer N+1 no rompen el paso.
    let futuro = r#"{"id":9,"kind":"skip","rel":"a","criterion":"presence",
                     "confidence":"unknown","reason":"unreadable","campo_del_futuro":true}"#;
    let s: SyncStep = serde_json::from_str(futuro).expect("tolerante");
    assert_eq!(s.reason, Some(SyncReason::Unreadable));
    assert!(s.reversal.is_none() && s.size.is_none());
}
