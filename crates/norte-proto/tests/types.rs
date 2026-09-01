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
fn full_fold_round_trips_on_the_wire() {
    // #145: el plegado de un directorio ext4/f2fs `+F` EXPANDE (ß -> ss), que
    // es una capability distinta de "no distingue caja".
    let c = Capabilities {
        flags: CapabilityFlags::CASE_PRESERVING | CapabilityFlags::FULL_FOLD,
        max_path: None,
    };
    let json = serde_json::to_value(c).expect("serializa");
    assert_eq!(json["flags"], "CASE_PRESERVING | FULL_FOLD");
    assert_eq!(roundtrip(&c), c);
}

#[test]
fn confined_writes_round_trips_on_the_wire() {
    // #164: lo responde `capabilities_at`, jamás `capabilities()` — depende
    // del mount, de la plataforma y del kernel en marcha.
    let c = Capabilities {
        flags: CapabilityFlags::CONFINED_WRITES,
        max_path: None,
    };
    let json = serde_json::to_value(c).expect("serializa");
    assert_eq!(json["flags"], "CONFINED_WRITES");
    assert_eq!(roundtrip(&c), c);
}

#[test]
fn posix_mode_round_trips_on_the_wire() {
    // #314: el nombre de este flag ES el wire, y renombrarlo falla EN
    // SILENCIO —un peer viejo ignora los nombres que no conoce (ADR 0004)—,
    // que es peor que renombrar un método. Por eso se congela aquí, como
    // `FULL_FOLD` y `CONFINED_WRITES`.
    let c = Capabilities {
        flags: CapabilityFlags::POSIX_MODE,
        max_path: None,
    };
    let json = serde_json::to_value(c).expect("serializa");
    assert_eq!(json["flags"], "POSIX_MODE");
    assert_eq!(roundtrip(&c), c);
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
        // 0.59.0 (#311). El schema también lo congela, pero ese rojo se
        // arregla regenerando; este obliga a tocar dos sitios a mano.
        (TaskKind::Checksum, "\"checksum\""),
        // 0.60.0 (#314).
        (TaskKind::SetMode, "\"set_mode\""),
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
        unreadable: None,
        unvisited: None,
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
        unreadable: None,
        unvisited: None,
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
fn escapes_root_round_trips_and_a_future_subtype_still_degrades() {
    // #164: una ruta relativa que se sale de su raíz confinada. NO es
    // NotFound — un caller que ve NotFound reintenta creando el padre, que es
    // justo lo que este subtipo existe para impedir.
    let e = Error::Conflict {
        conflict: ConflictKind::EscapesRoot,
    };
    let json = serde_json::to_value(&e).expect("serializa");
    assert_eq!(json["conflict"], "escapes_root");
    assert_eq!(roundtrip(&e), e);

    // Y la política N-1 sigue viva para el subtipo que venga después.
    let futuro: Error =
        serde_json::from_str(r#"{"kind": "conflict", "conflict": "subtipo_de_0_99"}"#).unwrap();
    assert_eq!(
        futuro,
        Error::Conflict {
            conflict: ConflictKind::Unknown
        }
    );
}

#[test]
fn stale_revision_round_trips_as_a_conflict() {
    // L2: la sesión de UI que se escribe contra una revisión que ya no es la
    // vigente. Es conflicto y no error de parámetros porque nada se escribió y
    // el caller arregla releyendo — y un cliente 0.47 lo degrada a `Unknown`,
    // que le deja exactamente la misma conducta.
    let e = Error::Conflict {
        conflict: ConflictKind::StaleRevision,
    };
    let json = serde_json::to_value(&e).expect("serializa");
    assert_eq!(json["conflict"], "stale_revision");
    assert_eq!(roundtrip(&e), e);
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
fn el_modo_de_apagado_no_degrada() {
    use norte_proto::methods;

    // El default reproduce EXACTAMENTE lo de hoy: un cliente que no conoce el
    // campo sigue APAGANDO el daemon, no relevándolo.
    let p: methods::DaemonShutdownParams = serde_json::from_str("{}").expect("todo-opcionales");
    assert_eq!(p.mode, methods::ShutdownMode::Stop);
    assert!(p.graceful, "y `graceful` no cambia de default");

    // `mode` y `graceful` son ejes ORTOGONALES: relevar dice quién viene
    // después, `graceful` dice qué se hace con las tasks vivas.
    let p: methods::DaemonShutdownParams =
        serde_json::from_str(r#"{"mode":"handover","graceful":false}"#).expect("json");
    assert_eq!(p.mode, methods::ShutdownMode::Handover);
    assert!(!p.graceful);

    // `"stop"` explícito también se acepta: nuestro emisor no lo escribe nunca
    // (`skip_serializing_if`), pero el schema lo publica como valor legal, así
    // que un cliente de terceros lo manda — y un `rename` futuro los rompería
    // con la suite en verde.
    let p: methods::DaemonShutdownParams =
        serde_json::from_str(r#"{"mode":"stop"}"#).expect("json");
    assert_eq!(p.mode, methods::ShutdownMode::Stop);

    // Y un modo que este binario no conoce NO se adivina. El resto de este wire
    // degrada ante un valor desconocido, y está bien: malinterpretarlo cuesta
    // una feature. Aquí cuesta apagar un daemon de una forma que el que llamó
    // no pidió, así que es la misma asimetría que `unknown_policies_are_hard_errors`.
    assert!(
        serde_json::from_str::<methods::ShutdownMode>(r#""teletransportar""#).is_err(),
        "un modo inventado no puede degradar a `stop`"
    );
}

/// La notificación lleva lo único que el cliente necesita para decidir: si
/// volver. Sin eso, un relevo y una parada son la misma conexión cerrada.
#[test]
fn going_away_dice_si_volver() {
    let n = norte_proto::methods::DaemonGoingAway { reconnect: true };
    let j = serde_json::to_value(n).expect("json");
    assert_eq!(j["reconnect"], serde_json::json!(true));
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
        detail: norte_proto::methods::ApprovalDetail {
            mode: Some(0o755),
            recursive: false,
            dir_mode: None,
        },
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
    // 0.43.0 (#207): acepta 0.43.x (N) y 0.42.x (N-1), rechaza 0.41.x (N-2) —
    // la ventana se DESPLAZA con el bump y no se ensancha, y que el bump sea
    // aditivo no la ensancha tampoco.
    //
    // Lo que la ventana compra aquí es distinto de lo que compraba en 0.42.0:
    // allí dos de los tres campos eran obligatorios y un informe N-1 ni
    // siquiera deserializaba. `SyncReason::NonInjectivePairing` sí degrada
    // (`#[serde(other)]` → `Unknown`), así que un cliente 0.42 leería el paso
    // sin romperse — pero leería «un motivo que no sé nombrar» sobre un `Skip`
    // que sí sabe no ejecutar, y eso es exactamente lo que la ventana N/N-1
    // permite que pase y N-2 no.
    //
    // 0.45.0 (ADR 0054): dos flags de capability y un subtipo de conflicto. Los
    // tres degradan solos —los nombres desconocidos se ignoran (ADR 0004) y el
    // subtipo cae en `Unknown` (ADR 0005)—, y aun así la ventana se DESPLAZA:
    // un cliente 0.43 que no conoce `CONFINED_WRITES` no sabe que una escritura
    // pudo ir sin confinar, así que no puede avisar de ello.
    //
    // 0.46.0 (roadmap ítem 10): `daemon.going_away` y `DaemonShutdownParams.mode`.
    // El caso más claro de por qué la ventana se desplaza aunque el bump sea
    // aditivo: un cliente 0.45 ignora la notificación —que es lo que ADR 0004
    // le manda hacer— y por tanto NO se entera de que venía un relevo. No se
    // rompe; se queda reconectando contra un socket muerto, que es justo el
    // comportamiento que 0.46 existe para arreglar.
    //
    // 0.47.0 (roadmap ítem 11): `rar` entra en `ARCHIVE_FORMATS`. No mueve un
    // byte de ningún mensaje: mueve qué schemes compuestos se pueden OFRECER.
    // Un cliente 0.46 no los ofrece —su whitelist no los trae— y se queda sin
    // la funcionalidad, que es la misma clase de pérdida silenciosa que
    // desplaza la ventana en los bumps anteriores.
    //
    // Lo que NO es cierto, y aquí se decía (#247): que un 0.46 «no forma» ese
    // path. `VPath::parse` no consulta la whitelist —solo `archive_compose` lo
    // hace—, así que un `rar+file:///a.rar/!/x` guardado en un marcador, en el
    // historial o en el cuerpo de una sesión lo parsea sin queja y se queda
    // con un scheme desconocido y un `!` literal. Falla al pedirlo, que es
    // aguas abajo y sin corromper nada; el bump sigue siendo MINOR.
    //
    // 0.48.0 (L2, la sesión de UI): `session.get` y `session.put` con sus
    // cuatro tipos. Aditivo —ningún tipo existente cambia de forma—, y aun así
    // la ventana se DESPLAZA por la razón de siempre: un cliente 0.47 no
    // conoce `session.*`, así que arranca sin la pantalla que dejó y jamás la
    // escribe. No se rompe; pierde en silencio justo lo que esta fase existe
    // para conservar.
    //
    // 0.49.0 (#139): `fs.dir_size` y `TaskKind::DirSize`. Aditivo por partida
    // doble —un método que un cliente viejo no forma y una variante de kind que
    // su `serde(other)` degrada desde 0.10—, y la ventana se desplaza por lo de
    // siempre: un cliente 0.48 no sabe preguntar cuánto ocupa una carpeta.
    //
    // 0.50.0 (#132): `archive.pack`, `archive.test`, `file.split` y
    // `file.combine`, con sus tipos y sus cuatro kinds. Aditivo igual, y la
    // ventana se desplaza igual: un cliente 0.49 no sabe empaquetar. Lo que
    // NO cambia es el provider de archivos —sigue `READ_ONLY`, ADR 0018—, así
    // que no hay ninguna operación vieja que se comporte distinto.
    //
    // 0.51.0 (#247): NI un tipo ni un campo nuevos, y aun así bump — lo que
    // cambia es lo que `session.put` acepta. Un `version` que el core no sabe
    // leer se rehúsa con `Unsupported` en vez de escribirse, porque escribirlo
    // dejaba la sesión «del futuro» desde el arranque siguiente y sin
    // persistencia para siempre. La ventana se desplaza por lo de siempre y en
    // la dirección menos habitual: contra un daemon 0.50 no se pierde
    // funcionalidad, se pierde la PROTECCIÓN.
    //
    // 0.52.0 (#163): `SyncBlockerKind::IllegalDestName`. Aditivo sobre un enum
    // `#[serde(other)]`, así que un cliente 0.51 lo degrada a `Unknown` — y un
    // bloqueo que no se entiende SIGUE bloqueando, que es la degradación que
    // hace falta. Lo que se pierde contra un daemon viejo es la comprobación,
    // no la corrección.
    // 0.53.0 (#251, #265, #282): tres campos opcionales, y los tres desplazan
    // la ventana por el mismo motivo — lo que se pierde contra un peer viejo
    // es una COMPROBACIÓN, no la corrección. `TaskProgress.unreadable` a cero
    // es lo que un daemon 0.52 sabía decir, así que un `fs.dir_size` contra él
    // sigue sin poder avisar de que su número es una cota inferior;
    // `PluginLoadError.dir_bytes` ausente deja la fila del error sin poder
    // marcar que se convirtió; y sin `expected_digest` el daemon concede lo
    // que tiene en vez de lo que se leyó.
    // 0.54.0 (#295): la identidad opaca del directorio que el humano miró,
    // viajando con la petición que escribe en él. Contra un daemon 0.53 no
    // hay ancla que retener, así que un cliente 0.54 no manda ninguna y la
    // escritura hace lo de 0.53 —se confina igual y no se comprueba la
    // identidad—: se pierde la comprobación, no la corrección.
    // 0.55.0 (#279): `Error::ApprovalGone`, que dice cuál de las tres formas
    // de «esa aprobación ya no está» ocurrió. Degrada solo —la categoría cae
    // en `Unknown` (ADR 0004)— y aun así la ventana se DESPLAZA: contra un
    // daemon 0.54 las tres siguen llegando como el error genérico de antes,
    // así que un cliente 0.55 no puede distinguir «llegaste tarde» de «tu clic
    // no llegó» y tiene que seguir dando el consejo prudente.
    // 0.56.0 (#264): `connection.list`. Un método nuevo que un cliente viejo
    // no llama, así que degrada solo; lo que desplaza la ventana es que
    // contra un daemon 0.55 no hay selector de conexiones que ofrecer.
    // Conectar no se pierde: sigue siendo navegar a una URL.
    // 0.57.0 (#290): `fs.create`, un fichero vacío como Task. Método nuevo que
    // un cliente viejo no llama y kind nuevo que degrada a `Unknown`, así que
    // no rompe nada; lo que desplaza la ventana es que contra un daemon 0.56
    // un frontend SIN TERMINAL no puede ofrecer «editar uno nuevo» — no hay
    // forma de crear el fichero, y lanzar un editor a que lo cree al guardar
    // es justo lo que una ventana no puede hacer.
    // 0.58.0 (#250): `archive.pack_report`. Método nuevo que un cliente viejo
    // no llama, y la ventana se desplaza en la dirección de 0.51.0. La pérdida
    // hay que contarla en la dirección que el handshake PERMITE, que es una
    // sola —cliente 0.57 contra daemon 0.58; al revés el cliente se rechaza
    // entero en `initialize`—: ese cliente empaqueta igual, con las mismas
    // entradas y los mismos bytes, y se queda sin el AVISO de que alguno de
    // esos nombres significa otra cosa al extraerlo en Windows.
    //
    // Lo que este informe NO lleva son las colisiones por plegado: esas no se
    // empaquetan (`archive.pack` falla con `Exists` antes de escribir un byte),
    // porque ahí sí DESAPARECE un fichero al extraer.
    // 0.59.0 (#311): `fs.checksum` y `fs.checksum_report`. Dos métodos nuevos
    // que un cliente viejo no llama, más un `TaskKind` que degrada a `Unknown`.
    // Aquí no hay degradación PARCIAL que contar —ni un campo que se ignore en
    // silencio—: un cliente 0.58 contra un daemon 0.59 se queda sin la
    // comprobación entera, que es lo que desplaza la ventana. Lo único que ve
    // del bump es una Task ajena que no sabe nombrar, como ya le pasa con
    // `Compare` o `DirSize`.
    //
    // 0.60.0 (#314): `fs.set_mode`, su `TaskKind` y la capability `POSIX_MODE`.
    // Un cliente 0.59 no llama al método, así que se queda sin poder cambiar
    // permisos —la superficie que tenía era de solo mirar, y sigue siéndolo—; y
    // del flag nuevo no ve nada, porque los nombres desconocidos se ignoran al
    // parsear (ADR 0004). Que no se rompa nada es justo lo que la ventana N/N-1
    // permite, y N-2 no.
    //
    // 0.61.0 (#314): el `detail` de una aprobación. Aditivo —se omite cuando no
    // dice nada, así que el JSON de las demás ops no cambia—, y la ventana se
    // desplaza porque contra un daemon 0.60 la pregunta de un `set-mode` no
    // puede decir QUÉ modo se va a fijar, que es la mitad de esa decisión.
    // 0.62.0 (#315, #121): `recursive`/`dir_mode` en `fs.set_mode` y `names`
    // en `ai.rename_plan`. Los tres son ALCANCE, no comprobaciones: un cliente
    // 0.61 no los manda, así que cambia permisos sobre las rutas exactas —lo
    // que ya esperaba— y pide el plan del directorio entero. Nada deja de
    // comprobarse; lo que no se estrecha es el alcance, y la ventana se
    // desplaza igual porque ese cliente no puede pedir ninguna de las dos
    // cosas.
    // 0.63.0 (#325): `Error::SecretNeeded` y `connection.provide_secret`. Un
    // cliente 0.62 degrada el error a `Unknown` y no llama al método, así que
    // enseña un fallo donde el nuevo abre un diálogo — o sea que no puede abrir
    // esa conexión, que es EXACTAMENTE lo que ya le pasaba. Aquí no se pierde
    // ninguna comprobación ni se ensancha ningún alcance; lo que ese cliente no
    // tiene es la única forma de contestar la pregunta, y por eso la ventana se
    // desplaza igual.
    // 0.64.0 (#322): la notificación `connection.failed`. Un cliente 0.63 no la
    // conoce y la descarta en silencio (ADR 0004), o sea que se queda como
    // estaba: el fallo le llega como categoría y la frase que lo explica no.
    // No pierde ninguna comprobación —nadie decide con esa frase, es para
    // leer— y aun así la ventana se DESPLAZA, porque ese cliente no puede
    // enseñar el diagnóstico que el nuevo sí enseña.
    assert!(version_compatible(PROTOCOL_VERSION, "0.64.9"), "N");
    assert!(version_compatible(PROTOCOL_VERSION, "0.63.0"), "N-1");
    assert!(
        !version_compatible(PROTOCOL_VERSION, "0.62.9"),
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
            manifest_digest: None,
        }],
        errors: vec![PluginLoadError {
            dir: "/plugins/broken".into(),
            reason: "manifiesto inválido".into(),
            dir_bytes: None,
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
        manifest_digest: None,
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
        paired_under: None,
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
    use norte_proto::methods::{CompareCriterion, CompareReason, CompareVerdict, PairTransform};
    let v: CompareVerdict = serde_json::from_str("\"teleported\"").expect("degrades");
    assert_eq!(v, CompareVerdict::Unknown);
    let c: CompareCriterion = serde_json::from_str("\"vibes\"").expect("degrades");
    assert_eq!(c, CompareCriterion::Unknown);
    let r: CompareReason = serde_json::from_str("\"gremlins\"").expect("degrades");
    assert_eq!(r, CompareReason::Unknown);
    let s: norte_proto::methods::Side = serde_json::from_str("\"middle\"").expect("degrades");
    assert_eq!(s, norte_proto::methods::Side::Unknown);
    // 0.42.0 (#152): el quinto fallback de esta familia. Y no basta con que
    // degrade — una transformación que este binario no sabe nombrar tampoco
    // sabe si es inocua, así que el default prudente se comprueba aquí.
    let pt: PairTransform = serde_json::from_str("\"transliteration\"").expect("degrades");
    assert_eq!(pt, PairTransform::Unknown);
    assert!(!pt.names_one_text());
}

/// 0.42.0 (#170, #195): los otros dos campos del bump son OBLIGATORIOS, y eso
/// es una decisión, no un descuido — un default sería una respuesta inventada
/// sobre si algo se puede deshacer, o sobre de qué raíz cuelga la ruta de un
/// fallo. Lo que la sostiene es la ventana N/N-1: la forma de 0.41 no llega
/// nunca a un decodificador 0.42 porque el handshake no negocia un cliente con
/// minor MAYOR que el servidor.
///
/// Este test pinea que, si llegara, se RECHAZA. `DestTrash` y `SyncStepKind` no
/// derivan `Default`, así que un `#[serde(default)]` a secas no compilaría —
/// pero un `#[serde(default = "...")]` con una función explícita sí, y es
/// exactamente el cambio que hay que notar (`protocol-guardian`, W4b MINOR-3).
#[test]
fn los_dos_campos_obligatorios_de_0_42_rechazan_la_forma_de_0_41() {
    use norte_proto::methods::{SyncFailure, SyncReportResult};
    let informe_0_41 = serde_json::json!({
        "done": 3, "failed": 0, "skipped": 0, "bytes": 4096,
        "failures": [], "batch_id": 12
    });
    assert!(
        serde_json::from_value::<SyncReportResult>(informe_0_41).is_err(),
        "sin `dest_trash` no hay informe: inventarla sería contestar «se puede \
         deshacer» sin saberlo"
    );
    let fallo_0_41 = serde_json::json!({"rel": "a.txt", "cause": "denied"});
    assert!(
        serde_json::from_value::<SyncFailure>(fallo_0_41).is_err(),
        "sin `kind` no hay fila de fallo: su ausencia dejaría el ancla otra vez \
         a la deducción que #195 cierra"
    );
}

/// 0.42.0 (#152): `paired_under` es OPCIONAL y se omite, así que la fila que
/// mandaba un daemon 0.41 sigue decodificando y una fila corriente sigue
/// viajando exactamente igual que antes del bump.
#[test]
fn paired_under_es_aditivo_en_las_dos_direcciones() {
    use norte_proto::methods::{CompareVerdict, PairTransform};
    let mut row = compare_row(
        CompareVerdict::Same,
        Some(compare_entry("file:///l/a")),
        Some(compare_entry("file:///r/a")),
    );
    let json = serde_json::to_value(&row).expect("json");
    assert!(
        json.as_object()
            .expect("objeto")
            .get("paired_under")
            .is_none(),
        "sin transformación no hay clave: {json}"
    );
    // Y la forma de 0.41.0 —sin la clave— sigue decodificando a `None`.
    let back: norte_proto::methods::CompareRow = serde_json::from_value(json).expect("0.41 shape");
    assert_eq!(back, row);

    row.paired_under = Some(PairTransform::NormalizationSingleton);
    let json = serde_json::to_value(&row).expect("json");
    assert_eq!(
        json["paired_under"],
        serde_json::json!("normalization_singleton")
    );
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
    assert!(p.max_depth.is_none() && !p.follow_symlinks && p.descend_orphans.is_none());
    assert_eq!(roundtrip(&p), p);
}

/// `descend_orphans` acepta un lado, se OMITE cuando no se pidió —la petición
/// de un cliente que no lo conoce sigue siendo byte a byte la de 0.39.0— y una
/// ERRATA muere en el deserializador.
///
/// Eso último es el punto: si el campo fuera un [`Side`] (que degrada con
/// `serde(other)`), un `"lft"` llegaría como `Some(Side::Unknown)` —ningún
/// lado— y la comparación no descendería por ninguno, sirviendo en silencio un
/// conjunto de filas distinto del pedido. Con [`DescendSide`] lo rechaza el
/// deserializador de CUALQUIER peer, que es más fuerte que un chequeo que un
/// handler pueda olvidar (y el brazo embebido, que no pasa por handler alguno,
/// queda cubierto igual).
#[test]
fn descend_orphans_se_omite_cuando_no_se_pide_y_una_errata_no_degrada() {
    use norte_proto::methods::{DescendSide, FsCompareParams};
    let minimo = r#"{"left":"file:///a","right":"file:///b"}"#;
    let p: FsCompareParams = serde_json::from_str(minimo).expect("params mínimos");
    let json = serde_json::to_value(&p).expect("json");
    assert!(
        json.get("descend_orphans").is_none(),
        "un campo ausente no puede aparecer en el wire: {json}"
    );

    let pedido = r#"{"left":"file:///a","right":"file:///b","descend_orphans":"right"}"#;
    let p: FsCompareParams = serde_json::from_str(pedido).expect("params");
    assert_eq!(p.descend_orphans, Some(DescendSide::Right));
    assert_eq!(roundtrip(&p), p);

    for malo in [r#""lft""#, r#""unknown""#, r#""both""#] {
        let crudo =
            format!(r#"{{"left":"file:///a","right":"file:///b","descend_orphans":{malo}}}"#);
        assert!(
            serde_json::from_str::<FsCompareParams>(&crudo).is_err(),
            "{malo} no puede colar como «ningún lado»"
        );
    }
}

/// [`FsCompareParams`] y [`SyncCompareOptions`] son EL MISMO juego de opciones
/// de comparación con dos envoltorios: el de un método y el que un plan embebe.
/// Nada en el compilador los ata —son dos structs, y así se quedan (0.40.0 no
/// puede cambiar la forma ya publicada de `FsCompareParams` con un `flatten`)—,
/// así que un rung añadido a uno solo pasaría desapercibido hasta que un plan
/// comparase distinto que `fs.compare` sobre los mismos dos árboles.
///
/// El test los ata: mismos NOMBRES de campo y mismos VALORES para la misma
/// configuración, menos las dos raíces, que en un plan se llaman `source` y
/// `dest` y viven en `SyncPlanParams`.
#[test]
fn las_dos_caras_de_las_opciones_de_comparacion_no_divergen() {
    use norte_proto::methods::{CompareCriteria, DescendSide, FsCompareParams, SyncCompareOptions};
    // Todo POBLADO: los `Option` se omiten al serializar, así que un campo a
    // `None` aquí sería un campo que este test no mira.
    let criteria = CompareCriteria {
        size: true,
        mtime: false,
        hash: true,
    };
    let params = FsCompareParams {
        left: vpath("file:///a"),
        right: vpath("file:///b"),
        criteria,
        max_depth: Some(3),
        mtime_tolerance_ms: 0,
        follow_symlinks: true,
        descend_orphans: Some(DescendSide::Left),
    };
    let embebidas = SyncCompareOptions {
        criteria,
        max_depth: Some(3),
        mtime_tolerance_ms: 0,
        follow_symlinks: true,
        descend_orphans: Some(DescendSide::Left),
    };

    let mut del_metodo = serde_json::to_value(&params).expect("json");
    let objeto = del_metodo
        .as_object_mut()
        .expect("los params son un objeto");
    assert!(objeto.remove("left").is_some() && objeto.remove("right").is_some());
    assert_eq!(
        del_metodo,
        serde_json::to_value(&embebidas).expect("json"),
        "las opciones de comparación de un método y las de un plan han divergido"
    );

    // Y los DEFAULTS, que es por donde divergirían sin que los nombres se
    // movieran: `FsCompareParams` los toma campo a campo (`serde(default …)`)
    // y `SyncCompareOptions` de un `Default` escrito a mano. Si se separan, un
    // plan con `"compare": {}` compararía distinto que un `fs.compare` sin
    // opciones, con los dos tipos idénticos en forma.
    let minimos: FsCompareParams =
        serde_json::from_str(r#"{"left":"file:///a","right":"file:///b"}"#).expect("params");
    let mut por_defecto = serde_json::to_value(&minimos).expect("json");
    let objeto = por_defecto.as_object_mut().expect("objeto");
    assert!(objeto.remove("left").is_some() && objeto.remove("right").is_some());
    assert_eq!(
        por_defecto,
        serde_json::to_value(SyncCompareOptions::default()).expect("json"),
        "los defaults de las dos caras han divergido"
    );
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
        dest_rel: None,
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

/// La papelera del destino va daemon→client, así que degrada — y lo que
/// degrada no promete nada: `restores()` es `false` para el valor desconocido,
/// que es la dirección segura (un diálogo que no sabe si algo vuelve no puede
/// decir que vuelve).
#[test]
fn an_unknown_dest_trash_degrades_and_promises_nothing() {
    use norte_proto::methods::DestTrash;
    let t: DestTrash = serde_json::from_value(serde_json::json!("quantum")).expect("degrada");
    assert_eq!(t, DestTrash::Unknown);
    assert!(!t.restores());
    // Y las tres respuestas de verdad, con la única que devuelve algo aparte.
    assert!(DestTrash::Restorable.restores());
    assert!(!DestTrash::Opaque.restores());
    assert!(!DestTrash::Absent.restores());
}

/// El cierre de un plan SIN `dest_trash` no se decodifica, y eso es
/// deliberado: un default sería inventar si algo se puede deshacer. La misma
/// decisión que los contadores nuevos de `SyncCounts` en el spool, pinada aquí
/// para que quitarla cueste borrar un test.
#[test]
fn a_plan_that_does_not_say_which_trash_the_destination_has_is_refused() {
    use norte_proto::methods::SyncPlanDone;
    let mut v = serde_json::json!({
        "task_id": 7,
        "plan_hash": "1".repeat(64),
        "counts": {
            "create_dir": 0, "copy": 1, "overwrite": 0, "delete_tree": 0,
            "skip": 0, "unknown_kind": 0, "irreversible": 0, "bytes": 10,
            "unmeasured_steps": 0
        },
        "blockers": [],
        "blockers_total": 0,
        "executable": true
    });
    assert!(
        serde_json::from_value::<SyncPlanDone>(v.clone()).is_err(),
        "sin papelera declarada no hay plan que aprobar"
    );
    v["dest_trash"] = serde_json::json!("absent");
    serde_json::from_value::<SyncPlanDone>(v).expect("con ella, sí");
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

/// `type_mismatch_dir` es el único bloqueo cuyo lado no se deduce de su clase, y
/// a la vez el único en el que el lado ES la frase que se pinta. Sin él no hay
/// nada que enseñar, así que la invariante se enuncia (y NO se rechaza al
/// deserializar: un bloqueo malformado degrada, no mata la lista).
#[test]
fn a_type_mismatch_dir_without_a_side_has_nothing_to_say() {
    use norte_proto::methods::{RelPath, Side, SyncBlocker, SyncBlockerKind};
    let mut b = SyncBlocker {
        rel: RelPath::parse_wire("build").expect("rel"),
        kind: SyncBlockerKind::TypeMismatchDir,
        side: Some(Side::Left),
    };
    assert!(b.shape_is_consistent());
    b.side = Some(Side::Right);
    assert!(b.shape_is_consistent());
    b.side = None;
    assert!(!b.shape_is_consistent());
    // El deserializador NO lo rechaza: llega, y quien lo lea decide.
    let crudo = serde_json::json!({"rel": "build", "kind": "type_mismatch_dir"});
    let venido: SyncBlocker = serde_json::from_value(crudo).expect("degrada, no muere");
    assert!(!venido.shape_is_consistent());

    // Los demás no deben un lado: el suyo se deduce de la clase, o no hay.
    for kind in [
        SyncBlockerKind::AmbiguousDest,
        SyncBlockerKind::OverlapDetected,
        SyncBlockerKind::DestReadOnly,
        SyncBlockerKind::DirTooLarge,
        SyncBlockerKind::Unknown,
    ] {
        assert!(
            SyncBlocker {
                rel: RelPath::default(),
                kind,
                side: None
            }
            .shape_is_consistent(),
            "{kind:?}"
        );
    }
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
        SyncBlockerKind::TypeMismatchDir,
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

/// `RelPath::under` es la ÚNICA forma de medir una ruta contra una raíz, así
/// que sus negativas se pinean aquí, en el crate que la publica.
///
/// Las tres que importan son negativas, y por el mismo motivo: fallar cerrado.
/// Un `Some` de más sería una ruta relativa inventada — y de ahí sale un
/// `include` que selecciona lo que el lector no marcó, o un paso que escribe
/// donde nadie miró.
#[test]
fn under_mide_por_segmentos_y_falla_cerrado() {
    use norte_proto::VPath;
    use norte_proto::methods::RelPath;

    let root = VPath::parse("file:///origen").expect("root");
    assert_eq!(
        RelPath::under(&root, &VPath::parse("file:///origen/sub/a.txt").expect("p"))
            .expect("cuelga")
            .to_wire(),
        "sub/a.txt"
    );
    // Por SEGMENTOS, no por prefijo de cadena: un hermano cuyo nombre EMPIEZA
    // por el de la raíz no cuelga de ella.
    assert!(RelPath::under(&root, &VPath::parse("file:///origen2/a.txt").expect("p")).is_none());
    // Más corta que la raíz.
    assert!(RelPath::under(&root, &VPath::parse("file:///").expect("p")).is_none());
    // Otro scheme, y otra authority — byte a byte, sin plegar: para `mem://` y
    // para un id de conexión de object storage la authority es un testigo
    // opaco, y plegarla juntaría dos conexiones distintas.
    assert!(RelPath::under(&root, &VPath::parse("mem:///origen/a.txt").expect("p")).is_none());
    let nas = VPath::parse("sftp://nas/d").expect("nas");
    assert!(RelPath::under(&nas, &VPath::parse("sftp://NAS/d/a.txt").expect("p")).is_none());
    // Y la raíz misma sale como la RAÍZ: es el valor CORRECTO de un
    // `SyncBlocker` que habla del árbol entero y el más destructivo de un paso,
    // así que decidir cuál de las dos cosas es le toca a quien llama.
    assert!(
        RelPath::under(&root, &root)
            .expect("la raíz cuelga de sí misma")
            .is_root()
    );
    // Los bytes no se tocan por el camino (regla dura 1).
    let hostil = VPath::parse("file:///origen/informe%FF%FE.dat").expect("p");
    assert_eq!(
        RelPath::under(&root, &hostil).expect("cuelga").segments()[0].as_bytes(),
        b"informe\xff\xfe.dat"
    );
}

/// `dest_rel` nombra la entrada del DESTINO cuando sus bytes no son los del
/// origen, y viaja con el mismo códec que `rel`: dos nombres que un humano
/// pinta igual —NFC contra NFD— son dos secuencias de bytes distintas, y el
/// paso las distingue.
///
/// La mitad NFC/NFD del caso se pinea AQUÍ y no en `sync_step.json`, a
/// propósito: las dos formas son UTF-8 válido, el códec de segmentos deja el
/// UTF-8 válido literal, y una fixture con las dos cadenas se leería como dos
/// cadenas IDÉNTICAS — un fallo invisible en la revisión, y a una
/// normalización de cualquier editor de dejar de comprobar nada. Aquí los
/// bytes van como escapes, que son ASCII y sobreviven a eso. La otra mitad —la
/// caja, que sí se ve— es la que congela la golden.
#[test]
fn dest_rel_carries_the_other_spelling_byte_for_byte() {
    use norte_proto::methods::{RelPath, StepReversal, SyncStep, SyncStepKind};
    let mut s = sync_step(
        SyncStepKind::Overwrite,
        Some(StepReversal::RestoreTrash),
        None,
    );
    s.rel = RelPath::parse_wire("caf\u{e9}").expect("nfc");
    s.dest_rel = Some(RelPath::parse_wire("cafe\u{301}").expect("nfd"));
    assert!(
        s.shape_is_consistent(),
        "dos ortografías distintas son exactamente el caso que el campo cubre"
    );
    assert_eq!(roundtrip(&s), s);
    // Las dos formas son UTF-8 VÁLIDO, así que el códec las deja literales y
    // las dos cadenas del JSON se pintan igual. Lo que las distingue son los
    // bytes, que es también lo único que el ejecutor va a abrir (regla dura 1).
    let json = serde_json::to_value(&s).expect("json");
    assert_ne!(json["rel"], json["dest_rel"]);
    let back: SyncStep = serde_json::from_value(json).expect("json");
    assert_eq!(
        back.rel.segments()[0].as_bytes(),
        "caf\u{e9}".as_bytes(),
        "5 bytes, uno de ellos de dos"
    );
    assert_eq!(
        back.dest_rel.expect("dest_rel").segments()[0].as_bytes(),
        "cafe\u{301}".as_bytes(),
        "6 bytes: la e y su tilde combinante"
    );
}

/// La regla normativa del campo, comprobable: `Some` SOLO cuando difiere. Un
/// `dest_rel` igual a `rel` no es peligroso, es ruido — y un consumidor que lo
/// vea sabe que quien lo produjo no aplicó la regla.
#[test]
fn a_dest_rel_that_repeats_rel_is_a_malformed_step() {
    use norte_proto::methods::{RelPath, StepReversal, SyncStep, SyncStepKind};
    let mut s = sync_step(
        SyncStepKind::Overwrite,
        Some(StepReversal::RestoreTrash),
        None,
    );
    s.dest_rel = Some(s.rel.clone());
    assert!(!s.shape_is_consistent());
    // Y no muere al deserializar, como el resto de la forma: un paso malo
    // degrada, jamás mata el lote de 256.
    let json = serde_json::to_value(&s).expect("json");
    let back: SyncStep = serde_json::from_value(json).expect("degrada, no muere");
    assert_eq!(
        back.dest_rel,
        Some(RelPath::parse_wire("sub/b.txt").expect("rel"))
    );
    assert!(!back.shape_is_consistent());
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
    for ausente in ["size", "reason", "dest_rel"] {
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

// ---------- SyncCounts::add (0.40.0) ----------

/// Un paso con la clase, el tamaño y la reversa que pida el test.
fn counted_step(
    kind: norte_proto::methods::SyncStepKind,
    size: Option<u64>,
) -> norte_proto::methods::SyncStep {
    use norte_proto::methods::{StepReversal, SyncReason, SyncStepKind};
    let (reversal, reason) = match kind {
        SyncStepKind::Skip => (None, Some(SyncReason::Unreadable)),
        _ => (Some(StepReversal::Delete), None),
    };
    let mut s = sync_step(kind, reversal, reason);
    s.size = size;
    s
}

#[test]
fn counts_add_up_per_kind_and_bytes_only_count_what_moves() {
    use norte_proto::methods::{SyncCounts, SyncStepKind};
    let mut c = SyncCounts::default();
    c.add(&counted_step(SyncStepKind::Copy, Some(10)));
    c.add(&counted_step(SyncStepKind::Overwrite, Some(20)));
    c.add(&counted_step(SyncStepKind::CreateDir, None));
    c.add(&counted_step(SyncStepKind::DeleteTree, None));
    c.add(&counted_step(SyncStepKind::Skip, None));
    assert_eq!(c.copy, 1);
    assert_eq!(c.overwrite, 1);
    assert_eq!(c.create_dir, 1);
    assert_eq!(c.delete_tree, 1);
    assert_eq!(c.skip, 1);
    assert_eq!(c.bytes, 30, "a delete and a skip move no bytes");
    assert_eq!(
        c.unmeasured_steps, 0,
        "no las mueve, así que tampoco son bytes que no se sepan"
    );
}

/// Un paso que no debería llevar tamaño y lo lleva no contamina el total: el
/// diálogo enseña bytes que se van a ESCRIBIR, y un borrado no escribe.
#[test]
fn a_size_on_a_step_that_moves_nothing_is_ignored_not_added() {
    use norte_proto::methods::{SyncCounts, SyncStepKind};
    let mut c = SyncCounts::default();
    c.add(&counted_step(SyncStepKind::DeleteTree, Some(4096)));
    c.add(&counted_step(SyncStepKind::Skip, Some(4096)));
    c.add(&counted_step(SyncStepKind::CreateDir, Some(4096)));
    assert_eq!((c.bytes, c.unmeasured_steps), (0, 0));
}

#[test]
fn a_step_with_no_size_is_counted_apart_and_never_as_zero() {
    // Un huérfano no se hidrata (#157) y `file://` lista sin tamaño, así que
    // esto es el caso NORMAL y no el raro. Un cero confiado en el diálogo de
    // aprobación sería mentira.
    use norte_proto::methods::{SyncCounts, SyncStepKind};
    let mut c = SyncCounts::default();
    c.add(&counted_step(SyncStepKind::Copy, Some(10)));
    c.add(&counted_step(SyncStepKind::Copy, None));
    c.add(&counted_step(SyncStepKind::Copy, None));
    assert_eq!(c.bytes, 10);
    assert_eq!(c.unmeasured_steps, 2);
    assert_eq!(c.copy, 3, "an unmeasured file is still a file to copy");
}

#[test]
fn irreversible_steps_are_counted_separately_because_the_dialog_leads_with_them() {
    use norte_proto::methods::{StepReversal, SyncCounts, SyncReason, SyncStepKind};
    let mut irreversible = sync_step(
        SyncStepKind::Overwrite,
        Some(StepReversal::Irreversible),
        Some(SyncReason::NoTrashOnTarget),
    );
    irreversible.size = Some(5);
    let mut c = SyncCounts::default();
    c.add(&irreversible);
    c.add(&counted_step(SyncStepKind::Copy, Some(5)));
    assert_eq!(c.irreversible, 1);
    assert_eq!(c.overwrite, 1);
    assert_eq!(c.bytes, 10);
}

/// Un paso de un daemon N+1 no entra en el contador de ninguna clase CONOCIDA
/// —eso mentiría sobre lo que el plan hace— pero se cuenta igual, porque no
/// contarlo mentiría sobre cuánto plan hay.
#[test]
fn an_unknown_kind_is_counted_as_unknown_and_not_dropped() {
    use norte_proto::methods::{StepReversal, SyncCounts, SyncReason, SyncStep, SyncStepKind};
    let s: SyncStep = serde_json::from_value(serde_json::json!({
        "id": 3, "kind": "teleport", "rel": "a", "size": 99,
        "criterion": "size", "confidence": "certain",
        "reversal": "irreversible", "reason": "no_trash_on_target",
    }))
    .expect("degrada");
    assert_eq!(s.kind, SyncStepKind::Unknown);
    assert_eq!(s.reversal, Some(StepReversal::Irreversible));
    assert_eq!(s.reason, Some(SyncReason::NoTrashOnTarget));
    let mut c = SyncCounts::default();
    c.add(&s);
    assert_eq!(c.unknown_kind, 1);
    assert_eq!(c.irreversible, 1, "no saber qué hace no lo hace reversible");
    assert_eq!(
        (c.copy, c.overwrite, c.create_dir, c.delete_tree, c.skip),
        (0, 0, 0, 0, 0)
    );
    assert_eq!(
        (c.bytes, c.unmeasured_steps),
        (0, 0),
        "no se sabe qué escribe, así que no se le atribuyen bytes"
    );
}

/// `exact_bytes` es el `Option<u64>` que `bytes` no es: el total, o nada.
#[test]
fn exact_bytes_is_the_total_or_nothing_at_all() {
    use norte_proto::methods::{SyncCounts, SyncStepKind};
    let mut c = SyncCounts::default();
    c.add(&counted_step(SyncStepKind::Copy, Some(10)));
    assert_eq!(c.exact_bytes(), Some(10));
    c.add(&counted_step(SyncStepKind::Copy, None));
    assert_eq!(
        c.exact_bytes(),
        None,
        "con un paso sin medir no hay total exacto que dar"
    );
    assert_eq!(c.bytes, 10, "y la cota inferior sigue ahí");
    assert_eq!(SyncCounts::default().exact_bytes(), Some(0));
}

/// La invariante que un consumidor puede comprobar antes de fiarse de unos
/// contadores que no calculó él: solo copiar y sobrescribir mueven bytes.
#[test]
fn only_the_two_kinds_that_move_bytes_can_raise_unmeasured_steps() {
    use norte_proto::methods::{SyncCounts, SyncStepKind};
    let mut c = SyncCounts::default();
    for kind in [
        SyncStepKind::CreateDir,
        SyncStepKind::Copy,
        SyncStepKind::Overwrite,
        SyncStepKind::DeleteTree,
        SyncStepKind::Skip,
    ] {
        c.add(&counted_step(kind, None));
    }
    assert_eq!(c.unmeasured_steps, 2);
    assert!(c.unmeasured_steps <= c.copy + c.overwrite);
}

/// Sumar no puede matar la Task que está planificando: satura.
#[test]
fn the_counters_saturate_instead_of_panicking() {
    use norte_proto::methods::{SyncCounts, SyncStepKind};
    let mut c = SyncCounts {
        bytes: u64::MAX - 1,
        copy: u64::MAX,
        ..SyncCounts::default()
    };
    c.add(&counted_step(SyncStepKind::Copy, Some(10)));
    assert_eq!(c.bytes, u64::MAX);
    assert_eq!(c.copy, u64::MAX);
}

/// L2: la sesión viaja como documento OPACO. El round trip conserva el body
/// entero —incluido un kind que ningún binario declara— porque nadie lo
/// interpreta por el camino.
#[test]
fn session_round_trips_an_opaque_body() {
    use norte_proto::methods::Session;
    let body = serde_json::json!({
        "version": 1,
        "layouts": { "default": { "kind": "kind-que-nadie-declara", "params": { "x": 1 } } },
        "slots": {}
    });
    let s = Session {
        version: 1,
        revision: 7,
        body: body.clone(),
    };
    let ida = serde_json::to_string(&s).expect("serializa");
    let vuelta: Session = serde_json::from_str(&ida).expect("deserializa");
    assert_eq!(vuelta.revision, 7);
    assert_eq!(vuelta.body, body, "el body vuelve entero, sin normalizar");
}

/// El tope del body es del PROTOCOLO, no una constante suelta del daemon: el
/// cliente necesita el mismo número para decidir qué tirar antes de
/// reintentar.
#[test]
fn session_body_max_es_un_mebibyte() {
    assert_eq!(norte_proto::methods::SESSION_BODY_MAX, 1024 * 1024);
}

/// `owner` viaja en el GET: el segundo cliente recibe una copia y tiene que
/// saber que lo es antes de intentar escribir.
#[test]
fn session_get_result_dice_quien_es_la_duena() {
    use norte_proto::methods::{Session, SessionGetResult};
    let r = SessionGetResult {
        session: Session {
            version: 1,
            revision: 0,
            body: serde_json::json!({}),
        },
        owner: false,
    };
    let v = serde_json::to_value(&r).expect("serializa");
    assert_eq!(v["owner"], serde_json::json!(false));
}

/// Una sesión nunca escrita es el Default: revisión 0 y sin esquema. El store
/// del core la construye así, y el cliente distingue «no hay» de «falló».
#[test]
fn session_default_es_revision_cero() {
    let s = norte_proto::methods::Session::default();
    assert_eq!(s.revision, 0);
    assert_eq!(s.version, 0);
}
