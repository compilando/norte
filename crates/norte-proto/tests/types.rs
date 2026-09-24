//! Deterministic tests for the protocol types (M0 phase 4): serde
//! roundtrip, tolerance to unknown fields (forward-compat N/N-1), and
//! the semantics of each type. The byte-exact wire lives in `golden_types.rs`.

use norte_proto::{
    AttrValue, Capabilities, CapabilityFlags, ConflictKind, Entry, EntryKind, Error, TaskId,
    TaskKind, TaskProgress, TaskState, VPath,
};

fn vpath(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid test wire")
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

/// (0.30.0, ADR 0039 §4) A malformed attribute key is DISCARDED when
/// decoding — never an error, same as a malformed cell degrades to
/// `Unknown`: a bad key costs that key, never the entry or the page.
/// The filter lives in the TYPE, not in every caller, because an id ends up
/// as a config id and a lookup key downstream.
#[test]
fn entry_discards_malformed_attribute_keys() {
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
    let e: Entry = serde_json::from_str(json).expect("a bad key must NOT break the entry");

    let keys: Vec<&str> = e.attrs.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec!["posix.mode", "s3.storage_class"],
        "only well-formed, namespaced ids survive"
    );
    assert_eq!(e.attrs["posix.mode"], AttrValue::Uint(33188));
}

/// (0.30.0, ADR 0039 §5) The map is BOUNDED when decoding: a client can
/// request at most `ATTRS_MAX_REQUEST` ids, so a bigger map is a buggy or
/// hostile peer. The LOWEST keys in byte order survive, so the SET of ids
/// that survives does not depend on the order the peer serialized in — JSON
/// objects are unordered (RFC 8259 §4). Note: here the keys are DISTINCT;
/// the case of a repeated key (last-wins) is covered by
/// `entry_repeated_attribute_key_is_last_wins`.
#[test]
fn entry_bounds_the_attribute_map_deterministically() {
    use norte_proto::attrs::ATTRS_MAX_REQUEST;

    let ids: Vec<String> = (0..40).map(|i| format!("test.attr_{i:02}")).collect();
    let cells = |order: &dyn Fn(&mut Vec<&String>)| {
        let mut keys: Vec<&String> = ids.iter().collect();
        order(&mut keys);
        let body: Vec<String> = keys
            .iter()
            .map(|id| format!("\"{id}\":{{\"uint\":1}}"))
            .collect();
        let json = format!(
            r#"{{"path":"file:///a","kind":"file","attrs":{{{}}}}}"#,
            body.join(",")
        );
        let e: Entry = serde_json::from_str(&json).expect("a fat map must NOT break the entry");
        e.attrs.into_keys().collect::<Vec<String>>()
    };

    let expected: Vec<String> = {
        let mut v = ids.clone();
        v.sort();
        v.truncate(ATTRS_MAX_REQUEST);
        v
    };
    assert_eq!(expected.len(), ATTRS_MAX_REQUEST);

    let ascending = cells(&|c| c.sort());
    let descending = cells(&|c| c.sort_by(|a, b| b.cmp(a)));
    assert_eq!(ascending, expected, "the lowest in bytes are kept");
    assert_eq!(
        descending, ascending,
        "the result does NOT depend on the wire key order"
    );
}

/// (0.30.0, rust-review MAJOR 1) A REPEATED key in the same object resolves
/// last-wins — like any JSON parser, and like the derived `BTreeMap` this
/// deserializer replaces. The determinism test permutes DISTINCT keys and
/// does not see this case: without the "the key is already in the map"
/// shortcut, the kept value depended on whether the map was full when the
/// duplicate arrived.
#[test]
fn entry_repeated_attribute_key_is_last_wins() {
    use norte_proto::attrs::ATTRS_MAX_REQUEST;

    fn decode(pairs: &[(String, u64)]) -> Entry {
        let body: Vec<String> = pairs
            .iter()
            .map(|(k, v)| format!("\"{k}\":{{\"uint\":{v}}}"))
            .collect();
        let json = format!(
            r#"{{"path":"file:///a","kind":"file","attrs":{{{}}}}}"#,
            body.join(",")
        );
        serde_json::from_str(&json).expect("a repeated key must NOT break the entry")
    }

    // BELOW the cap: the duplicate wins, wherever it is.
    let dup_at_end = decode(&[
        ("a.k00".to_owned(), 1),
        ("b.k00".to_owned(), 9),
        ("a.k00".to_owned(), 2),
    ]);
    let dup_at_start = decode(&[
        ("a.k00".to_owned(), 1),
        ("a.k00".to_owned(), 2),
        ("b.k00".to_owned(), 9),
    ]);
    assert_eq!(dup_at_end.attrs["a.k00"], AttrValue::Uint(2));
    assert_eq!(dup_at_end.attrs, dup_at_start.attrs);

    // AT the cap, repeating the HIGHEST key (the one pruning would evict): the
    // map is already full when the duplicate arrives in one order and not the
    // other, and the result must still be the same.
    let full: Vec<(String, u64)> = (0..ATTRS_MAX_REQUEST)
        .map(|i| (format!("a.k{i:02}"), 1))
        .collect();
    let highest = format!("a.k{:02}", ATTRS_MAX_REQUEST - 1);

    let mut dup_after = full.clone();
    dup_after.push((highest.clone(), 2));

    let mut dup_before = vec![(highest.clone(), 1), (highest.clone(), 2)];
    dup_before.extend(full.iter().filter(|(k, _)| *k != highest).cloned());

    let a = decode(&dup_after);
    let b = decode(&dup_before);
    assert_eq!(a.attrs.len(), ATTRS_MAX_REQUEST);
    assert_eq!(
        a.attrs[&highest],
        AttrValue::Uint(2),
        "last-wins also with a full map"
    );
    assert_eq!(
        a.attrs, b.attrs,
        "same members in a different order → same result, values included"
    );
}

/// (0.30.0, rust-review MAJOR 2) The filter is ONE-way: `attrs` is a public
/// field with no constructor and serialization does NOT filter, so an
/// `Entry` built in-process with an invalid id or above the cap EMITS what
/// it carries and comes back DIFFERENT. Pinned here so nobody assumes
/// round-trip identity: the producer's bug must stay visible at the
/// boundary that validates it (block 2), not laundered by the serializer.
#[test]
fn entry_built_in_process_is_not_filtered_and_does_not_roundtrip() {
    use norte_proto::attrs::ATTRS_MAX_REQUEST;

    let with_invalid_id = Entry {
        attrs: std::collections::BTreeMap::from([("MODE".to_owned(), AttrValue::Uint(1))]),
        ..sample_entry()
    };
    let wire = serde_json::to_string(&with_invalid_id).expect("serializable");
    assert!(
        wire.contains("MODE"),
        "serialization must NOT filter: the producer's bug travels: {wire}"
    );
    assert_ne!(
        roundtrip(&with_invalid_id),
        with_invalid_id,
        "and coming back, the invalid key is gone"
    );

    let fat = Entry {
        attrs: (0..ATTRS_MAX_REQUEST + 5)
            .map(|i| (format!("test.attr_{i:02}"), AttrValue::Uint(i as u64)))
            .collect(),
        ..sample_entry()
    };
    assert_eq!(
        roundtrip(&fat).attrs.len(),
        ATTRS_MAX_REQUEST,
        "above the cap it is emitted whole but decoded bounded"
    );
}

/// Regression guard for the filter: a normal entry (valid ids, below the
/// cap) round-trips EXACTLY, attributes included.
#[test]
fn entry_with_valid_attributes_roundtrips_exactly() {
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
    // Forward-compat: an N+1 core can add fields; an N client does not blow up.
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
    // Backward-compat: absent optional fields → None, not an error.
    let json = r#"{"path": "file:///a", "kind": "other"}"#;
    let e: Entry = serde_json::from_str(json).expect("opcionales ausentes valen None");
    assert_eq!(e.size, None);
    assert_eq!(e.mtime_ms, None);
}

#[test]
fn entry_mtime_pre_epoch() {
    // mtime_ms is i64: pre-1970 dates exist on real filesystems.
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
    // ADR 0004 policy: a capability is an announcement — a well-formed N+1
    // name is ignored (never exploited), never blows up on an N client.
    let json = r#"{"flags": "RENAME_ATOMIC | FUTURE_FLAG", "max_path": null}"#;
    let c: Capabilities = serde_json::from_str(json).expect("unknown name is ignored");
    assert_eq!(c.flags, CapabilityFlags::RENAME_ATOMIC);
}

#[test]
fn full_fold_round_trips_on_the_wire() {
    // #145: the fold of an ext4/f2fs `+F` directory EXPANDS (ß -> ss), which
    // is a capability distinct from "does not distinguish case".
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
    // #164: `capabilities_at` answers it, never `capabilities()` — it depends
    // on the mount, the platform, and the running kernel.
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
    // #314: this flag's name IS the wire, and renaming it fails SILENTLY
    // —an old peer ignores names it does not know (ADR 0004)—, which is
    // worse than renaming a method. So it is frozen here, like `FULL_FOLD`
    // and `CONFINED_WRITES`.
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
    // Nameless bits do NOT travel: bitflags::parser::from_str would silently
    // retain them via hex; the wire always rejects them.
    for bad in ["0x20", "0x3", "RENAME_ATOMIC | 0x40", "0X20"] {
        let json = format!(r#"{{"flags": "{bad}", "max_path": null}}"#);
        assert!(
            serde_json::from_str::<Capabilities>(&json).is_err(),
            "hex should have failed: {bad}"
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
            "malformed should have failed: {bad}"
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
    // N/N-1 tolerance: unknown state → Unknown, NOT terminal (the client
    // keeps listening), never a deserialization error.
    let s: TaskState = serde_json::from_str(r#"{"kind": "future_state"}"#).unwrap();
    assert!(!s.is_terminal());
    // With an extra payload too.
    let s: TaskState = serde_json::from_str(r#"{"kind": "future_state", "detail": 5}"#).unwrap();
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
        // 0.59.0 (#311). The schema also freezes it, but that red is fixed
        // by regenerating; this one forces touching two places by hand.
        (TaskKind::Checksum, "\"checksum\""),
        (TaskKind::DirUsage, "\"dir_usage\""),
        // 0.60.0 (#314).
        (TaskKind::SetMode, "\"set_mode\""),
    ] {
        assert_eq!(serde_json::to_string(&kind).unwrap(), wire);
    }
}

#[test]
fn task_kind_unknown_is_tolerant() {
    // An N-1 client receives a future kind → Unknown, not a parse error
    // (forward-compat, same as TaskState::Unknown).
    let k: TaskKind = serde_json::from_str("\"teleport\"").expect("tolerant");
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
    // Unknown totals (walk still in progress): None, never a faked 0.
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
    // #164: a relative path that escapes its confined root. It is NOT
    // NotFound — a caller that sees NotFound retries by creating the parent,
    // which is exactly what this subtype exists to prevent.
    let e = Error::Conflict {
        conflict: ConflictKind::EscapesRoot,
    };
    let json = serde_json::to_value(&e).expect("serializes");
    assert_eq!(json["conflict"], "escapes_root");
    assert_eq!(roundtrip(&e), e);

    // And the N-1 policy stays alive for whatever subtype comes next.
    let future: Error =
        serde_json::from_str(r#"{"kind": "conflict", "conflict": "subtype_of_0_99"}"#).unwrap();
    assert_eq!(
        future,
        Error::Conflict {
            conflict: ConflictKind::Unknown
        }
    );
}

/// The 0.84.0 token, pinned by name like its two siblings.
///
/// The fixture and the schema↔golden cross-check also cover it, but a named
/// test is what makes renaming it surface where it is read. And here it
/// matters more than usual: `destination_gone` and `escapes_root` are
/// similar enough for someone to conflate them, and they are not the same
/// thing — one says the path leads outside its root (the SHAPE of the
/// path), the other that the folder is gone.
#[test]
fn destination_gone_round_trips_as_a_conflict() {
    let e = Error::Conflict {
        conflict: ConflictKind::DestinationGone,
    };
    let json = serde_json::to_value(&e).expect("serializes");
    assert_eq!(json["conflict"], "destination_gone");
    assert_eq!(roundtrip(&e), e);
    assert_ne!(
        json["conflict"],
        serde_json::to_value(Error::Conflict {
            conflict: ConflictKind::EscapesRoot
        })
        .expect("serializes")["conflict"],
        "two distinct subtypes cannot share a token"
    );
}

#[test]
fn stale_revision_round_trips_as_a_conflict() {
    // L2: the UI session written against a revision that is no longer
    // current. It is a conflict and not a param error because nothing was
    // written and the caller fixes it by re-reading — and a 0.47 client
    // degrades it to `Unknown`, which leaves it with exactly the same
    // behavior.
    let e = Error::Conflict {
        conflict: ConflictKind::StaleRevision,
    };
    let json = serde_json::to_value(&e).expect("serializes");
    assert_eq!(json["conflict"], "stale_revision");
    assert_eq!(roundtrip(&e), e);
}

#[test]
fn error_unknown_kind_degrades() {
    // N/N-1 tolerance: unknown category → generic error, not a crash.
    let e: Error = serde_json::from_str(r#"{"kind": "future_quota"}"#).unwrap();
    assert_eq!(e, Error::Unknown);
    let with_payload: Error =
        serde_json::from_str(r#"{"kind": "future_quota", "limit": 9}"#).unwrap();
    assert_eq!(with_payload, Error::Unknown);
}

#[test]
fn conflict_unknown_subtype_degrades_nested() {
    // N/N-1 tolerance (ADR 0005): an unknown conflict subtype INSIDE a known
    // Error::Conflict degrades to Unknown, does not blow up.
    let e: Error =
        serde_json::from_str(r#"{"kind": "conflict", "conflict": "future_subtype"}"#).unwrap();
    assert_eq!(
        e,
        Error::Conflict {
            conflict: ConflictKind::Unknown
        }
    );
}

#[test]
fn rename_collision_unknown_kind_degrades_nested() {
    // Same criterion as `conflict_unknown_subtype_degrades_nested` (ADR
    // 0005), and here what is at stake is the WHOLE plan: a verdict from a
    // 0.37 daemon (which `version_compatible` accepts against a 0.36
    // client) must not leave the human with no plan to review — it degrades
    // that row, not the document.
    use norte_proto::methods::{FsRenameBatchPlanResult, RenameCollisionKind};
    // The hash is a VALID one (64 lowercase hex): with `"00"` this test used
    // to pass for the wrong reason, and along the way pinned that any string
    // is a plan hash. What is being demonstrated here is the verdict's
    // fallback, nothing more.
    let json = format!(
        r#"{{"steps":[],"collisions":[{{"pair_index":3,"name":"a","kind":"future_verdict"}}],
            "executable":false,"plan_hash":"{}"}}"#,
        "ab".repeat(32)
    );
    let r: FsRenameBatchPlanResult =
        serde_json::from_str(&json).expect("an unknown verdict must NOT break the plan");
    assert_eq!(r.collisions.len(), 1);
    assert_eq!(r.collisions[0].kind, RenameCollisionKind::Unknown);
    assert_eq!(r.collisions[0].name.as_bytes(), b"a");
    // And the row is STILL addressable: `pair_index` does not depend on
    // `kind`, which is exactly what makes the fallback useful instead of
    // decorative.
    assert_eq!(r.collisions[0].pair_index, 3);
}

/// (0.36.0) N-1 tolerance over a type that WAS already alive: a 0.35 daemon
/// emits `PolicyUndoReportResult` without `batch_stuck` or
/// `compensations_lost`, and that report must keep deserializing here. The
/// absence means exactly what it looks like — that daemon did not know how
/// to undo batches, so it could not leave any half-done.
///
/// What is being demonstrated is the `serde(default)` of
/// `compensations_lost`: the goldens ALWAYS carry the key (it travels even
/// at zero), so without this test the attribute could be deleted and the
/// suite would stay green.
#[test]
fn policy_undo_report_result_shape_0_35_tolerated() {
    use norte_proto::methods::{FsRenameBatchReportResult, PolicyUndoReportResult};
    let old_shape = r#"{
        "undone": 4,
        "skipped_irreversible": 1,
        "skipped_created_no_trash": 0
    }"#;
    let r: PolicyUndoReportResult =
        serde_json::from_str(old_shape).expect("shape 0.35.x tolerated");
    assert_eq!(r.undone, 4);
    assert!(r.batch_stuck.is_none());
    assert_eq!(r.compensations_lost, 0);

    // And the BATCH report, born in this same bump, holds up the same way:
    // its three absent optionals are "none of that happened", not a parse
    // error.
    let minimal: FsRenameBatchReportResult =
        serde_json::from_str(r#"{"applied":3,"rolled_back":0}"#).expect("minimal tolerated");
    assert!(minimal.stuck.is_none());
    assert!(minimal.uncertain.is_none());
    assert!(minimal.failed_pair.is_none());
    assert_eq!(minimal.compensations_lost, 0);
}

/// `plan_hash` is validated at DESERIALIZATION, so a wrong shape dies at the
/// edge (`-32602` for the daemon) and never reaches the comparator, which is
/// where it would turn into a lying `PlanStale`.
#[test]
fn malformed_plan_hash_dies_on_the_wire() {
    use norte_proto::methods::{FsRenameBatchParams, PlanHash};
    let params = |hash: &str| format!(r#"{{"dir":"file:///d","pairs":[],"plan_hash":"{hash}"}}"#);
    // Uppercase: the SAME hash, written another way — rejected so that two
    // forms of the same value do not compare differently depending on who
    // wrote it.
    for bad in [
        "00",
        &"AB".repeat(32),
        &"ab".repeat(33),
        &format!("{}g", "a".repeat(63)),
    ] {
        assert!(
            serde_json::from_str::<FsRenameBatchParams>(&params(bad)).is_err(),
            "{bad} is not a plan hash"
        );
    }
    let good = "ab".repeat(32);
    let ok: FsRenameBatchParams = serde_json::from_str(&params(&good)).expect("64 lowercase hex");
    assert_eq!(ok.plan_hash, PlanHash::parse(&good).expect("hash"));
    // Round-trip: what comes out is exactly what went in.
    assert_eq!(ok.plan_hash.as_str(), good);
}

#[test]
fn shutdown_mode_does_not_degrade() {
    use norte_proto::methods;

    // The default reproduces EXACTLY today's behavior: a client that does
    // not know the field still SHUTS DOWN the daemon, does not hand it over.
    let p: methods::DaemonShutdownParams = serde_json::from_str("{}").expect("all-optional");
    assert_eq!(p.mode, methods::ShutdownMode::Stop);
    assert!(p.graceful, "and `graceful` does not change its default");

    // `mode` and `graceful` are ORTHOGONAL axes: handing over says who comes
    // next, `graceful` says what happens to the tasks still alive.
    let p: methods::DaemonShutdownParams =
        serde_json::from_str(r#"{"mode":"handover","graceful":false}"#).expect("json");
    assert_eq!(p.mode, methods::ShutdownMode::Handover);
    assert!(!p.graceful);

    // Explicit `"stop"` is also accepted: our emitter never writes it
    // (`skip_serializing_if`), but the schema publishes it as a legal value,
    // so a third-party client sends it — and a future `rename` would break
    // them with the suite green.
    let p: methods::DaemonShutdownParams =
        serde_json::from_str(r#"{"mode":"stop"}"#).expect("json");
    assert_eq!(p.mode, methods::ShutdownMode::Stop);

    // And a mode this binary does not know is NOT guessed. The rest of this
    // wire degrades on an unknown value, and that is fine: misreading it
    // costs a feature. Here it costs shutting down a daemon in a way the
    // caller did not ask for, so it is the same asymmetry as
    // `unknown_policies_are_hard_errors`.
    assert!(
        serde_json::from_str::<methods::ShutdownMode>(r#""teleport""#).is_err(),
        "a made-up mode cannot degrade to `stop`"
    );
}

/// The notification carries the one thing the client needs to decide:
/// whether to come back. Without it, a handover and a stop are the same
/// closed connection.
#[test]
fn going_away_says_whether_to_reconnect() {
    let n = norte_proto::methods::DaemonGoingAway { reconnect: true };
    let j = serde_json::to_value(n).expect("json");
    assert_eq!(j["reconnect"], serde_json::json!(true));
}

#[test]
fn unknown_policies_are_hard_errors() {
    // Deliberate asymmetry (ADR 0005): policies travel client→server as
    // mutating ORDERS — a core that does not understand them must reject
    // the request, never degrade to a default that does something else.
    assert!(serde_json::from_str::<norte_proto::CollisionPolicy>(r#""future_policy""#).is_err());
    assert!(serde_json::from_str::<norte_proto::SymlinkPolicy>(r#""future_policy""#).is_err());
    // Resume/verify (0.6.0) are just as mutating: unknown value = error.
    assert!(serde_json::from_str::<norte_proto::ResumePolicy>(r#""future""#).is_err());
    assert!(serde_json::from_str::<norte_proto::VerifyPolicy>(r#""future""#).is_err());
}

#[test]
fn copy_params_absent_policies_default() {
    // The 0.1.0 wire shape ({"from","to"} without policies) is still valid:
    // absence = Fail/Preserve (M0's behavior).
    use norte_proto::methods::{FsCopyParams, FsMoveParams};
    let p: FsCopyParams =
        serde_json::from_str(r#"{"from": "file:///a", "to": "file:///b"}"#).unwrap();
    assert_eq!(p.on_collision, norte_proto::CollisionPolicy::Fail);
    assert_eq!(p.symlinks, norte_proto::SymlinkPolicy::Preserve);
    // resume/verify absent (0.5 client) = Off/Length = M1 contract (0.6.0).
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
fn delete_params_without_mode_is_trash() {
    // ADR 0009: the wire default is the SAFE one — a 0.2 client that does
    // not send mode gets the trash, never a surprise loss.
    use norte_proto::methods::FsDeleteParams;
    let p: FsDeleteParams = serde_json::from_str(r#"{"path": "file:///x"}"#).unwrap();
    assert_eq!(p.mode, norte_proto::DeleteMode::Trash);
    // An unknown mode is a hard error (a mutating order, like the policies).
    assert!(
        serde_json::from_str::<FsDeleteParams>(r#"{"path": "file:///x", "mode": "future_mode"}"#)
            .is_err()
    );
}

#[test]
fn error_display_is_english_and_stable() {
    // Display is for logs (frontends render by category, not by string).
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

/// Struct tolerance (ADR 0004) applies to the envelope: extra fields from a
/// newer protocol are ignored.
#[test]
fn envelope_ignores_unknown_fields() {
    use norte_proto::wire::{Message, Request};
    let r: Request = serde_json::from_str(
        r#"{"jsonrpc":"2.0","id":1,"method":"fs.list","params":null,"traceparent":"00-abc"}"#,
    )
    .unwrap();
    assert_eq!(r.method, "fs.list");
    // And structural classification is not thrown off by the extra field.
    let m: Message = serde_json::from_str(
        r#"{"jsonrpc":"2.0","id":1,"method":"fs.list","params":null,"extra":1}"#,
    )
    .unwrap();
    assert!(matches!(m, Message::Request(_)));
}

/// A `jsonrpc` other than "2.0" is REJECTED (a peer that does not speak the protocol).
#[test]
fn envelope_rejects_jsonrpc_other_than_2_0() {
    use norte_proto::wire::Request;
    for raw in [
        r#"{"jsonrpc":"1.0","id":1,"method":"m","params":null}"#,
        r#"{"jsonrpc":"3.0","id":1,"method":"m","params":null}"#,
        r#"{"id":1,"method":"m","params":null}"#,
    ] {
        assert!(
            serde_json::from_str::<Request>(raw).is_err(),
            "should have rejected: {raw}"
        );
    }
}

/// A response with result AND error (or neither) violates JSON-RPC:
/// `outcome` turns it into a protocol error, never interprets it.
#[test]
fn response_outcome_validates_xor() {
    use norte_proto::wire::{Response, RpcError, codes};
    let both: Response = serde_json::from_str(
        r#"{"jsonrpc":"2.0","id":1,"result":{},"error":{"code":-32000,"message":"x","data":null}}"#,
    )
    .unwrap();
    assert_eq!(both.outcome().unwrap_err().code, codes::INVALID_REQUEST);
    let neither: Response =
        serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":null,"error":null}"#).unwrap();
    assert_eq!(neither.outcome().unwrap_err().code, codes::INVALID_REQUEST);
    // The peer's error is delivered as is.
    let err = Response::err(None, RpcError::protocol(codes::PARSE_ERROR, "x"));
    assert_eq!(err.outcome().unwrap_err().code, codes::PARSE_ERROR);
}

/// The application error carries the WHOLE taxonomy in data — the frontends'
/// contract is data.kind, not code/message.
#[test]
fn application_rpc_error_carries_the_taxonomy_in_data() {
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
    // data from a newer protocol degrades through Error's fallback.
    let raw = r#"{"code":-32000,"message":"x","data":{"kind":"future_category"}}"#;
    let back: RpcError = serde_json::from_str(raw).unwrap();
    assert_eq!(back.data, Some(Error::Unknown));
}

// ---------- NDJSON framing (ADR 0011) ----------

#[test]
fn frame_decoder_chunks_and_tolerates() {
    use norte_proto::wire::FrameDecoder;
    let mut d = FrameDecoder::new();
    // Partial, then two complete ones in one push, with \r\n and a blank line.
    d.push(b"{\"a\"").unwrap();
    assert_eq!(d.next_frame(), None);
    d.push(b":1}\r\n\n{\"b\":2}\n{\"c\"").unwrap();
    assert_eq!(d.next_frame(), Some(b"{\"a\":1}".to_vec()));
    assert_eq!(d.next_frame(), Some(b"{\"b\":2}".to_vec()));
    assert_eq!(d.next_frame(), None, "the third one did not close");
    d.push(b":3}\n").unwrap();
    assert_eq!(d.next_frame(), Some(b"{\"c\":3}".to_vec()));
}

#[test]
fn frame_decoder_rejects_giant_frames() {
    use norte_proto::wire::{FrameDecoder, MAX_FRAME_BYTES};
    let mut d = FrameDecoder::new();
    let chunk = vec![b'x'; 1024 * 1024];
    let mut failed = false;
    for _ in 0..=(MAX_FRAME_BYTES / chunk.len()) {
        if d.push(&chunk).is_err() {
            failed = true;
            break;
        }
    }
    assert!(
        failed,
        "a never-ending frame must be cut off at MAX_FRAME_BYTES"
    );
}

#[test]
fn encode_frame_ends_in_newline_and_roundtrips() {
    use norte_proto::wire::{FrameDecoder, Request, RequestId, encode_frame};
    let req = Request {
        jsonrpc: norte_proto::wire::JsonRpcVersion,
        id: RequestId::Num(1),
        method: "fs.stat".into(),
        params: None,
    };
    let frame = encode_frame(&req).unwrap();
    assert_eq!(frame.last(), Some(&b'\n'));
    // serde_json escapes internal \n: a frame is ALWAYS one line.
    assert_eq!(
        frame.iter().position(|&b| b == b'\n'),
        Some(frame.len() - 1)
    );
    let mut d = FrameDecoder::new();
    d.push(&frame).unwrap();
    let back: Request = serde_json::from_slice(&d.next_frame().unwrap()).unwrap();
    assert_eq!(back, req);
}

// ---------- N/N-1 versioning (ADR 0011) ----------

#[test]
fn version_compatible_only_n_and_n_minus_1() {
    use norte_proto::methods::version_compatible;
    // 0.x: the minor is the effective major.
    assert!(version_compatible("0.4.0", "0.4.7"));
    assert!(version_compatible("0.4.0", "0.3.2"));
    assert!(!version_compatible("0.4.0", "0.2.9"));
    assert!(!version_compatible("0.4.0", "0.5.0"));
    assert!(!version_compatible("0.4.0", "1.4.0"));
    // Malformed: never compatible, never a panic.
    for v in ["", "0.4", "0.4.0.1", "a.b.c", "0.4.x", " 0.4.0"] {
        assert!(!version_compatible("0.4.0", v), "accepted {v:?}");
    }
}

/// Envelope tolerance: `params` ABSENT (not null) and `encodings` absent in
/// initialize — the receiver accepts absence (ADR 0004).
#[test]
fn envelope_tolerates_absences() {
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
    assert!(p.encodings.is_empty(), "encodings absent = empty = json");
}

/// Structural classification (guardian's M2/M3): valid JSON that is not an
/// envelope, and requests with an illegal id type — never silence.
#[test]
fn classify_distinguishes_invalid_from_illegal() {
    use norte_proto::wire::{MessageKind, classify};
    let j = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
    assert_eq!(classify(&j(r#"{"foo":1}"#)), MessageKind::Invalid);
    assert_eq!(classify(&j("[1,2]")), MessageKind::Invalid);
    // Illegal id (negative/fractional/null): still a Request — the server
    // answers INVALID_REQUEST instead of swallowing it.
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
    // A response with an error and no explicit id also classifies.
    assert_eq!(
        classify(&j(
            r#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"x"}}"#
        )),
        MessageKind::Response
    );
}

/// Strict semver in negotiation (guardian's m2): no `+`, no leading zeros,
/// no pre-release/build metadata — deliberate and pinned.
#[test]
fn version_compatible_is_strict_about_the_format() {
    use norte_proto::methods::version_compatible;
    for v in ["+0.4.0", "0.04.0", "0.4.00", "0.4.0-rc.1", "0.4.0+abc"] {
        assert!(!version_compatible("0.4.0", v), "accepted {v:?}");
    }
}

/// Tolerance (ADR 0004): `range` ABSENT in fs.read = None (default), not
/// only explicit `null` (ADR 0004).
#[test]
fn fs_read_params_tolerates_absent_range() {
    use norte_proto::methods::FsReadParams;
    let p: FsReadParams = serde_json::from_str(r#"{"path":"file:///x"}"#).expect("absent range");
    assert!(p.range.is_none());
}

/// N-1 compat (ADR 0017): a 0.7 client OMITS `limit`/`cursor`/`next_cursor`
/// (does not send `null`). The golden pins the canonical `null` from the 0.8
/// emitter; this one pins the other direction — ABSENT keys → None. Without
/// this, removing `#[serde(default)]` would pass every test and only break
/// the 0.7 ones.
#[test]
fn fs_list_params_tolerates_absent_cursor_and_limit() {
    use norte_proto::methods::FsListParams;
    let p: FsListParams =
        serde_json::from_str(r#"{"path":"file:///x"}"#).expect("absent limit/cursor");
    assert!(p.limit.is_none() && p.cursor.is_none());
}

#[test]
fn fs_list_result_tolerates_absent_next_cursor() {
    use norte_proto::methods::FsListResult;
    let r: FsListResult = serde_json::from_str(r#"{"entries":[]}"#).expect("absent next_cursor");
    assert!(r.next_cursor.is_none());
    // `skipped` (0.22, #93) absent = None; and a 0.22 emitter OMITS it when
    // it is None (skip_serializing_if — never `"skipped": null` on the wire).
    assert!(r.skipped.is_none());
    assert!(
        !serde_json::to_string(&r)
            .expect("serializable")
            .contains("skipped")
    );
    // And an UNKNOWN field (0.9 → 0.8) does not break deserialization.
    let r2: FsListResult = serde_json::from_str(r#"{"entries":[],"future_field":42}"#)
        .expect("unknown field tolerated");
    assert!(r2.entries.is_empty());
}

#[test]
fn agent_session_optional_roundtrip() {
    use norte_proto::methods::InitializeParams;
    // Absent = None (human frontend); a 0.10 client does not send it.
    let human: InitializeParams = serde_json::from_str(
        r#"{"client_info":{"name":"tui","version":"1"},"protocol_version":"0.11.0"}"#,
    )
    .expect("without agent_session");
    assert_eq!(human.agent_session, None);
    // Present = agent session.
    let agent = InitializeParams {
        client_info: norte_proto::methods::ClientInfo {
            name: "mcp".into(),
            version: "1".into(),
        },
        protocol_version: "0.11.0".into(),
        encodings: vec![],
        agent_session: Some("s1".into()),
    };
    let wire = serde_json::to_string(&agent).unwrap();
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
    // N-1 tolerance (0.36.0): a 0.35 server does not send `paths_total`, and
    // its absence falls to 0 = UNKNOWN, which is what that server could say.
    let old: PolicyApprovalRequired = serde_json::from_str(
        r#"{"approval_id":7,"op":"delete","paths":["file:///work/x"],"ttl_ms":30000}"#,
    )
    .expect("shape 0.35.x tolerated");
    assert_eq!(old.paths_total, 0);
    let dec: PolicyDecideParams =
        serde_json::from_str(r#"{"approval_id":7,"approve":true}"#).unwrap();
    assert!(dec.approve);
}

#[test]
fn current_version_window() {
    use norte_proto::PROTOCOL_VERSION;
    use norte_proto::methods::version_compatible;
    // 0.43.0 (#207): accepts 0.43.x (N) and 0.42.x (N-1), rejects 0.41.x
    // (N-2) — the window SHIFTS with the bump and does not widen, and the
    // bump being additive does not widen it either.
    //
    // What the window buys here differs from what it bought in 0.42.0:
    // there two of the three fields were required and an N-1 report would
    // not even deserialize. `SyncReason::NonInjectivePairing` DOES degrade
    // (`#[serde(other)]` → `Unknown`), so a 0.42 client would read the step
    // without breaking — but it would read "a reason I cannot name" over a
    // `Skip` that DOES know not to execute, and that is exactly what the
    // N/N-1 window allows to happen and N-2 does not.
    //
    // 0.45.0 (ADR 0054): two capability flags and one conflict subtype. All
    // three degrade on their own —unknown names are ignored (ADR 0004) and
    // the subtype falls to `Unknown` (ADR 0005)—, and the window still
    // SHIFTS: a 0.43 client that does not know `CONFINED_WRITES` does not
    // know a write may have gone unconfined, so it cannot warn about it.
    //
    // 0.46.0 (roadmap item 10): `daemon.going_away` and
    // `DaemonShutdownParams.mode`. The clearest case of why the window
    // shifts even though the bump is additive: a 0.45 client ignores the
    // notification —which is what ADR 0004 tells it to do— and so does NOT
    // learn a handover was coming. It does not break; it keeps
    // reconnecting to a dead socket, which is exactly the behavior 0.46
    // exists to fix.
    //
    // 0.47.0 (roadmap item 11): `rar` enters `ARCHIVE_FORMATS`. It does not
    // move a byte of any message: it moves which composed schemes can be
    // OFFERED. A 0.46 client does not offer them —its whitelist does not
    // carry them— and is left without the functionality, which is the
    // same class of silent loss that shifts the window in earlier bumps.
    //
    // What is NOT true, and was said here (#247): that a 0.46 "cannot
    // form" that path. `VPath::parse` does not consult the whitelist —only
    // `archive_compose` does—, so a `rar+file:///a.rar/!/x` saved in a
    // bookmark, in history, or in a session's body parses without
    // complaint and is left with an unknown scheme and a literal `!`. It
    // fails when requested, which is downstream and without corrupting
    // anything; the bump stays MINOR.
    //
    // 0.48.0 (L2, the UI session): `session.get` and `session.put` with
    // their four types. Additive —no existing type changes shape—, and the
    // window still SHIFTS for the usual reason: a 0.47 client does not
    // know `session.*`, so it starts without the screen it left and never
    // writes it. It does not break; it silently loses exactly what this
    // phase exists to preserve.
    //
    // 0.49.0 (#139): `fs.dir_size` and `TaskKind::DirSize`. Doubly
    // additive —a method an old client cannot form and a kind variant its
    // `serde(other)` degrades since 0.10—, and the window shifts for the
    // usual reason: a 0.48 client does not know how to ask how much space
    // a folder takes.
    //
    // 0.50.0 (#132): `archive.pack`, `archive.test`, `file.split` and
    // `file.combine`, with their types and their four kinds. Additive the
    // same way, and the window shifts the same way: a 0.49 client does not
    // know how to package. What does NOT change is the archive provider
    // —it stays `READ_ONLY`, ADR 0018—, so no old operation behaves
    // differently.
    //
    // 0.51.0 (#247): NEITHER a new type nor a new field, and still a bump
    // — what changes is what `session.put` accepts. A `version` the core
    // cannot read is refused with `Unsupported` instead of being written,
    // because writing it left the session "from the future" from the next
    // boot onward and without persistence forever. The window shifts for
    // the usual reason and in the less usual direction: against a 0.50
    // daemon no functionality is lost, PROTECTION is lost.
    //
    // 0.52.0 (#163): `SyncBlockerKind::IllegalDestName`. Additive over a
    // `#[serde(other)]` enum, so a 0.51 client degrades it to `Unknown` —
    // and a blocker that is not understood STILL blocks, which is the
    // degradation that is needed. What is lost against an old daemon is
    // the check, not correctness.
    // 0.53.0 (#251, #265, #282): three optional fields, and all three
    // shift the window for the same reason — what is lost against an old
    // peer is a CHECK, not correctness. `TaskProgress.unreadable` at zero
    // is what a 0.52 daemon knew how to say, so an `fs.dir_size` against it
    // still cannot warn that its number is a lower bound; an absent
    // `PluginLoadError.dir_bytes` leaves the error row unable to mark that
    // it was converted; and without `expected_digest` the daemon grants
    // what it has instead of what was read.
    // 0.54.0 (#295): the opaque identity of the directory the human looked
    // at, traveling with the request that writes into it. Against a 0.53
    // daemon there is no anchor to hold, so a 0.54 client sends none and
    // the write does what 0.53 did —it confines the same and identity is
    // not checked—: the check is lost, not correctness.
    // 0.55.0 (#279): `Error::ApprovalGone`, which says which of the three
    // forms of "that approval is no longer there" occurred. It degrades on
    // its own —the category falls to `Unknown` (ADR 0004)— and the window
    // still SHIFTS: against a 0.54 daemon all three still arrive as the
    // old generic error, so a 0.55 client cannot distinguish "you arrived
    // late" from "your click did not land" and has to keep giving the
    // cautious advice.
    // 0.56.0 (#264): `connection.list`. A new method an old client does
    // not call, so it degrades on its own; what shifts the window is that
    // against a 0.55 daemon there is no connection picker to offer.
    // Connecting is not lost: it is still navigating to a URL.
    // 0.57.0 (#290): `fs.create`, an empty file as a Task. A new method an
    // old client does not call and a new kind that degrades to `Unknown`,
    // so nothing breaks; what shifts the window is that against a 0.56
    // daemon a frontend WITHOUT A TERMINAL cannot offer "edit a new one" —
    // there is no way to create the file, and launching an editor to
    // create it on save is exactly what a window cannot do.
    // 0.58.0 (#250): `archive.pack_report`. A new method an old client
    // does not call, and the window shifts in the direction of 0.51.0.
    // The loss has to be counted in the direction the handshake ALLOWS,
    // which is a single one —a 0.57 client against a 0.58 daemon; the
    // other way the client is rejected entirely at `initialize`—: that
    // client still packages, with the same entries and the same bytes,
    // and is left without the WARNING that one of those names means
    // something else when extracted on Windows.
    //
    // What this report does NOT carry are fold collisions: those are not
    // packaged (`archive.pack` fails with `Exists` before writing a
    // byte), because there a file DOES disappear on extraction.
    // 0.59.0 (#311): `fs.checksum` and `fs.checksum_report`. Two new
    // methods an old client does not call, plus a `TaskKind` that
    // degrades to `Unknown`. There is no PARTIAL degradation to count
    // here —not a field silently ignored—: a 0.58 client against a 0.59
    // daemon is left without the entire check, which is what shifts the
    // window. All it sees of the bump is someone else's Task it cannot
    // name, as already happens with `Compare` or `DirSize`.
    //
    // 0.60.0 (#314): `fs.set_mode`, its `TaskKind`, and the `POSIX_MODE`
    // capability. A 0.59 client does not call the method, so it is left
    // unable to change permissions —the surface it had was look-only, and
    // stays so—; and it sees nothing of the new flag, because unknown
    // names are ignored when parsing (ADR 0004). That nothing breaks is
    // exactly what the N/N-1 window allows, and N-2 does not.
    //
    // 0.61.0 (#314): an approval's `detail`. Additive —omitted when it
    // says nothing, so the JSON of the other ops does not change—, and
    // the window shifts because against a 0.60 daemon a `set-mode`
    // question cannot say WHICH mode is about to be set, which is half of
    // that decision.
    // 0.62.0 (#315, #121): `recursive`/`dir_mode` in `fs.set_mode` and
    // `names` in `ai.rename_plan`. All three are SCOPE, not checks: a
    // 0.61 client does not send them, so it changes permissions on the
    // exact paths —what it already expected— and requests the plan for
    // the whole directory. Nothing stops being checked; what does not
    // narrow is the scope, and the window shifts the same way because
    // that client cannot ask for either of the two things.
    // 0.63.0 (#325): `Error::SecretNeeded` and `connection.provide_secret`.
    // A 0.62 client degrades the error to `Unknown` and does not call the
    // method, so it shows a failure where the new one opens a dialog —
    // meaning it cannot open that connection, which is EXACTLY what
    // already happened to it. No check is lost here nor is any scope
    // widened; what that client lacks is the only way to answer the
    // question, and that is why the window shifts the same way.
    // 0.64.0 (#322): the `connection.failed` notification. A 0.63 client
    // does not know it and silently discards it (ADR 0004), meaning it
    // stays as it was: the failure arrives as a category and the sentence
    // explaining it does not. It loses no check —nobody decides based on
    // that sentence, it is for reading— and the window still SHIFTS,
    // because that client cannot show the diagnosis the new one does
    // show.
    // 0.65.0 (#328): `log.tail` and `log.level`. Two new methods a 0.64
    // client does not call, so its log panel is left with its own
    // process's ring buffer — what it already had. No check is lost: the
    // cap that keeps a `suppaftp` TRACE from showing a password lives in
    // the ring's process, and that is why `log.level` is a METHOD, so an
    // old peer cannot skip it by not knowing it.
    //
    // The other direction does NOT count, and it is worth saying because
    // it is tempting to write it: a 0.65 client against a 0.64 daemon
    // never gets to try, because `version_compatible` does not negotiate
    // a client minor GREATER than the server's, and that client dies at
    // `initialize` with `VERSION_MISMATCH`. It is the same accounting
    // noted at 0.47.0.
    //
    // What DOES need attention is a daemon of this SAME version compiled
    // without the `logging` feature: it knows the methods, has no ring
    // buffer, and answers `METHOD_NOT_FOUND`. There the panel is left
    // with the local log and has to SAY why — silently degrading is
    // indistinguishable from a daemon that did nothing, which is the
    // confusion #326 started to fix.
    //
    // 0.66.0 (D4, ADR 0037): `SpanWire::bg` and
    // `PluginPreviewStyledParams::columns`, both optional and omitted
    // when absent. Additive, and the window SHIFTS for the usual reason:
    // a 0.65 client ignores `bg` (ADR 0004) and paints an image with half
    // its pixels, and does not send `columns`, so the guest picks a
    // width the viewer crops. Neither an error nor a warning — which is
    // the silent loss the N/N-1 window allows and N-2 does not.
    // 0.68.0 (#332): a 0.67 client ignores `refused` and says "the model
    // proposed no changes" where the plugin explained why. Imprecise, not
    // broken.
    // 0.69.0 (ADR 0100): a 0.68 client discards `plugin.notice` (ADR
    // 0004). The hook ran —the source is the journal— and its sentence
    // reached nobody; it also did not learn that a plugin's hooks were
    // turned off.
    // 0.70.0 (ADR 0101): a 0.69 client discards the `effect-denied` `kind`
    // and does not learn its policy is stopping a plugin from writing.
    // 0.71.0 (ADR 0104): a 0.70 client does not know how to request
    // `plugin.uninstall` and does not ask for it; it uninstalls via the
    // CLI as before. Nothing is lost.
    // 0.72.0 (ADR 0105): a 0.71 client does not send `kinds` nor read
    // `slot`: folders go without an icon and the icon is painted as a
    // badge. Ugly, not broken.
    // 0.73.0 (ADR 0107): a 0.72 client does not know how to request
    // `plugin.thumbnail` and does not ask for it; the viewer is left
    // without a thumbnail, which is what it already had.
    // 0.74.0 (phase 3): a 0.73 client does not know how to request
    // `plugin.panel_render` and does not ask for it, so a panel slot is
    // left with its own notice — the same thing it shows when the plugin
    // that paints it is uninstalled. And it reads an empty `panels` in
    // every `PluginInfo`, meaning "this plugin offers no panels", which is
    // exactly what that client could already know before panels existed.
    // 0.75.0 (phase 4): a 0.74 client does not know how to request
    // `fs.dir_usage` and does not ask for it, so it is left without a disk
    // map — the screen it already had. And if it sees someone else's Task
    // in `task.list`, its `TaskKind` falls to `Unknown` via
    // `serde(other)`: it paints it as a task it cannot name, with its
    // progress and its cancel button, instead of failing the parse.
    // 0.76.0 (phase 7): a 0.75 client does not know how to request
    // `journal.list` or `journal.undo_after`, so it does not ask for them
    // and is left without a timeline — what it already had. Undo is still
    // what it already knew how to do: `policy.undo_session` for a whole
    // agent session, with the same report. Nothing old changes shape: the
    // two methods are new and no existing type gains or loses a field.
    // 0.77.0 (phase 8): a 0.76 client does not know how to request an
    // organize plan nor apply it, so it does not ask for it and is left
    // with batch renaming, which is what it already had. Nothing old
    // changes shape: three new methods and five new types, and no
    // existing type gains or loses a field.
    // 0.78.0 (phase 9): a 0.77 client does not know how to request
    // `session.release`, so it does not ask for it; what it loses is the
    // handoff between frontends, and the command itself declares itself
    // unavailable with its reason instead of pretending to work. Nothing
    // old changes shape: one new method and one new type, and no existing
    // type gains or loses a field.
    // 0.79.0: `policy.undo_report` answers `NotFound` for an unknown id
    // instead of `INVALID_PARAMS`. No type changes; a 0.78 client that
    // distinguished the case by the code no longer recognizes it and
    // reads it as the generic error it already knew how to read.
    // 0.80.0: `journal.undo_after` gains `upto_seq`, optional. A 0.79
    // daemon ignores it and undoes without a ceiling (as before); a 0.79
    // client does not send it.
    // 0.81.0: `fs.search` gains ten filters, all optional. And here the
    // window buys something different from usual: a 0.80 daemon that
    // ignored one of them would not silently stop filtering, it would
    // return the SUPERSET — the whole tree instead of what was requested,
    // and with the same face. That is why the SDK does not send them and
    // refuses with `Unsupported` naming the filter. The other way around
    // is harmless: a 0.80 client does not send them and the daemon reads
    // them as absent, which is 0.80's search.
    // 0.82.0: `task.pause` and `task.resume`. A 0.81 daemon answers
    // `METHOD_NOT_FOUND` and the SDK reports it as `Unsupported`; a 0.81
    // client sees `Paused`, which it already knew how to read as
    // non-terminal.
    // 0.83.0: `queued` in copy and move, and `task.move`. A 0.82 daemon
    // ignores `queued` —parallel, as usual— and does not know `task.move`.
    // 0.84.0: `ConflictKind::DestinationGone`. A 0.83 client degrades it
    // to `Unknown` and shows plain "conflict": it loses the sentence, not
    // the protection — the daemon does the check, so the task fails all
    // the same and files do not end up in a folder nobody can see
    // anymore. And even being additive, the window SHIFTS: a 0.83 client
    // cannot offer "recreate the folder and retry", because it does not
    // know that is what happened.
    assert!(version_compatible(PROTOCOL_VERSION, "0.84.9"), "N");
    assert!(version_compatible(PROTOCOL_VERSION, "0.83.0"), "N-1");
    assert!(
        !version_compatible(PROTOCOL_VERSION, "0.82.9"),
        "N-2 outside the window"
    );
}

#[test]
fn connection_degraded_roundtrip_and_omitted_detail() {
    use norte_proto::methods::ConnectionDegraded;
    let without = ConnectionDegraded {
        scheme: "ftp".into(),
        host: "h".into(),
        reason: "tls-auth-rejected".into(),
        detail: None,
    };
    assert_eq!(
        serde_json::to_value(&without).unwrap(),
        serde_json::json!({"scheme":"ftp","host":"h","reason":"tls-auth-rejected"}),
    );
    let with = ConnectionDegraded {
        detail: Some("server rejected AUTH TLS".into()),
        ..without.clone()
    };
    let back: ConnectionDegraded =
        serde_json::from_value(serde_json::to_value(&with).unwrap()).unwrap();
    assert_eq!(back, with);
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
    // plugin.list carries no params (empty object, the task.list pattern).
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
            panels: vec![],
            has_help: false,
            manifest_digest: None,
        }],
        errors: vec![PluginLoadError {
            dir: "/plugins/broken".into(),
            reason: "invalid manifest".into(),
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

/// (P1, 0.26.0) N-1 tolerance: an old peer that emits `PluginInfo` WITHOUT
/// `description`/`commands` (0.25.x shape) must keep deserializing here —
/// both fields fall to their default (`None`/`vec![]`), never an error.
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
    let info: PluginInfo = serde_json::from_str(old_shape).expect("shape 0.25.x tolerated");
    assert_eq!(info.description, None);
    assert!(info.commands.is_empty());
}

/// (P1, 0.26.0) Backward byte stability: when `description` is `None` (the
/// default, and what a plugin with no manifest description produces today),
/// the key does NOT go out on the wire — an N-1 peer that only knows the
/// 0.25.x shape sees exactly what it saw before, except for the new
/// `commands` (additive, always present even when empty).
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
        panels: vec![],
        has_help: false,
        manifest_digest: None,
    };
    let wire = serde_json::to_string(&info).unwrap();
    assert!(
        !wire.contains("description"),
        "description:None must not serialize: {wire}"
    );
    assert!(
        wire.contains(r#""commands":[]"#),
        "commands is additive but ALWAYS present (no skip_if when empty): {wire}"
    );
    assert!(
        wire.contains(r#""columns":[]"#),
        "columns (0.28.0, G3c) is additive but ALWAYS present (no skip_if when empty): {wire}"
    );
}

/// (G3c, 0.28.0) N-1 tolerance: a peer that emits `PluginInfo` in the
/// 0.27.x shape (without `columns`) must keep deserializing here — it falls
/// to its default (`vec![]`), same criterion as
/// `plugin_info_old_shape_tolerance` for `description`/`commands` in 0.26.0.
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
    let info: PluginInfo = serde_json::from_str(shape_027).expect("shape 0.27.x tolerated");
    assert!(info.columns.is_empty());
}

#[test]
fn plugin_run_command_roundtrip() {
    use norte_proto::methods::{PluginRunCommandParams, PluginRunCommandResult};
    // With explicit `arg`: exact round-trip.
    let p = PluginRunCommandParams {
        id: "org.norte.demo".into(),
        command: "greet".into(),
        arg: "world".into(),
    };
    let back: PluginRunCommandParams =
        serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
    assert_eq!(back, p);
    // `arg` absent → `""` (the default), and without skip_serializing_if it
    // ALWAYS goes out on the wire: a client that omits it gets `arg:""` when
    // re-serialized.
    let without_arg: PluginRunCommandParams =
        serde_json::from_str(r#"{"id":"org.norte.demo","command":"greet"}"#).unwrap();
    assert_eq!(without_arg.arg, "");
    assert_eq!(
        serde_json::to_string(&without_arg).unwrap(),
        r#"{"id":"org.norte.demo","command":"greet","arg":""}"#
    );
    let r: PluginRunCommandResult = serde_json::from_str(r#"{"output":"hello, world"}"#).unwrap();
    assert_eq!(r.output, "hello, world");
}

#[test]
fn plugin_preview_roundtrip() {
    use norte_proto::methods::{PluginPreview, PluginPreviewParams, PluginPreviewResult};
    // Params with a VPath: exact round-trip.
    let p = PluginPreviewParams {
        path: vpath("file:///a.txt"),
    };
    let back: PluginPreviewParams =
        serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
    assert_eq!(back, p);
    // POPULATED result (Some): exact round-trip, flatten at the root level.
    let full = PluginPreviewResult {
        preview: Some(PluginPreview {
            plugin_id: "org.norte.md".into(),
            plugin_name: "Markdown Preview".into(),
            output: "<h1>Title</h1>".into(),
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
    // 0.29.0 (#101): `lossy` is additive — an N-1 wire (0.28.x) WITHOUT the
    // field deserializes to `false` (no warning, the safe direction).
    let n1: PluginPreviewResult = serde_json::from_str(
        r#"{"plugin_id":"org.norte.md","plugin_name":"Markdown Preview","output":"x"}"#,
    )
    .expect("shape 0.28.x tolerated");
    assert!(!n1.preview.unwrap().lossy, "lossy absent = false (N-1)");
    // EMPTY result: `{}` deserializes to None and re-serializes to `{}` (no
    // previewer applies — the frontend falls back to the raw view).
    let none: PluginPreviewResult = serde_json::from_str("{}").unwrap();
    assert_eq!(none.preview, None);
    assert_eq!(serde_json::to_string(&none).unwrap(), "{}");
    // PARTIAL state: the Rust type makes it UNBUILDABLE (preview is an
    // `Option<PluginPreview>` of required fields); on the wire an object with
    // only some fields collapses to `None` (no preview, safe) — never a
    // `plugin_id` without `output`.
    let partial: PluginPreviewResult =
        serde_json::from_str(r#"{"plugin_id":"x"}"#).expect("partial deserializes");
    assert_eq!(partial.preview, None, "a partial preview falls to None");
}

/// `plugin.preview_styled` (0.27.0, G3, ADR 0037): same all-or-nothing
/// pattern as `plugin_preview_roundtrip` above, with `lines` of spans
/// instead of a flat `output`.
#[test]
fn plugin_preview_styled_roundtrip() {
    use norte_proto::methods::{
        PluginPreviewStyled, PluginPreviewStyledParams, PluginPreviewStyledResult, SpanWire,
    };
    let p = PluginPreviewStyledParams {
        path: vpath("file:///a.rs"),
        columns: None,
    };
    let back: PluginPreviewStyledParams =
        serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
    assert_eq!(back, p);
    // POPULATED result: exact round-trip, flatten at the root level (like
    // `plugin_preview_result`).
    let full = PluginPreviewStyledResult {
        preview: Some(PluginPreviewStyled {
            plugin_id: "org.norte.demo".into(),
            plugin_name: "Demo Previewer".into(),
            lines: vec![vec![SpanWire {
                text: "fn".into(),
                role: Some("match".into()),
                fg: None,
                bg: None,
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
    // EMPTY result: `{}` deserializes to None and re-serializes to `{}`.
    let none: PluginPreviewStyledResult = serde_json::from_str("{}").unwrap();
    assert_eq!(none.preview, None);
    assert_eq!(serde_json::to_string(&none).unwrap(), "{}");
    // PARTIAL state: unbuildable in Rust (the three fields go together in
    // `PluginPreviewStyled`); a partial object from the wire collapses to
    // `None`.
    let partial: PluginPreviewStyledResult =
        serde_json::from_str(r#"{"plugin_id":"x"}"#).expect("partial deserializes");
    assert_eq!(
        partial.preview, None,
        "a partial styled preview falls to None"
    );
}

/// `SpanWire`/`DecorationWire` (0.27.0, G3, ADR 0037): `role`/`fg`/`badge`
/// are independent `Option`s with `skip_serializing_if` — when missing,
/// they do NOT go out on the wire (minimal payload, same treatment as
/// `description` in `PluginInfo`), and a shape that only carries
/// `text`/empty tolerates their absence when deserializing.
#[test]
fn span_wire_and_decoration_wire_optionals_are_independent_and_omitted() {
    use norte_proto::methods::{DecorationWire, SpanWire};
    let bare = SpanWire {
        text: "fn".into(),
        role: None,
        fg: None,
        bg: None,
    };
    assert_eq!(
        serde_json::to_string(&bare).unwrap(),
        r#"{"text":"fn"}"#,
        "absent role/fg do not go out on the wire"
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
        "a decoration with neither badge nor role serializes to an empty object, not null"
    );
    let back: DecorationWire = serde_json::from_str("{}").unwrap();
    assert_eq!(back, empty_decoration);
}

/// `plugin.decorate`/`plugin.column_values` (0.27.0, G3, ADR 0037):
/// POSITIONAL 1:1 with `paths`, never a key→value map — an element with no
/// data for that path is still present (not omitted), and the order is
/// preserved byte-exact, including a HOSTILE (non-UTF-8) name.
#[test]
fn plugin_decorate_and_column_values_are_positional() {
    use norte_proto::methods::{
        DecorationSlot, DecorationWire, PluginColumnValuesParams, PluginColumnValuesResult,
        PluginDecorateParams, PluginDecorateResult, PluginDecorations,
    };
    let paths = vec![
        vpath("file:///repo/a.rs"),
        vpath("file:///repo/informe%FF%FE.dat"),
    ];
    let dp = PluginDecorateParams {
        paths: paths.clone(),
        kinds: Vec::new(),
    };
    let back: PluginDecorateParams =
        serde_json::from_str(&serde_json::to_string(&dp).unwrap()).unwrap();
    assert_eq!(back, dp);

    let dr = PluginDecorateResult {
        plugins: vec![PluginDecorations {
            plugin_id: "org.norte.git".into(),
            slot: DecorationSlot::default(),
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
        "one decoration per path, without omitting the one with no badge"
    );
    let back: PluginDecorateResult =
        serde_json::from_str(&serde_json::to_string(&dr).unwrap()).unwrap();
    assert_eq!(back, dr);

    let cvp = PluginColumnValuesParams {
        column_id: "git-status".into(),
        paths: paths.clone(),
        plugin_id: None,
    };
    // Absent is NOT emitted: the request from a client that names no plugin
    // is byte-for-byte the 0.34 one (#120).
    assert!(
        !serde_json::to_string(&cvp).unwrap().contains("plugin_id"),
        "an absent plugin_id must not appear on the wire"
    );
    let cvp_scoped = PluginColumnValuesParams {
        plugin_id: Some("org.norte.git".into()),
        ..cvp.clone()
    };
    let back: PluginColumnValuesParams =
        serde_json::from_str(&serde_json::to_string(&cvp_scoped).unwrap()).unwrap();
    assert_eq!(back.plugin_id.as_deref(), Some("org.norte.git"));
    // `Some("")` (a real cell, empty string) and `None` (the column does not
    // apply to that entry) must be distinguishable on the wire — `values:
    // Vec<Option<String>>`, not `Vec<String>` (protocol-guardian MAJOR
    // applied).
    let cvr = PluginColumnValuesResult {
        values: vec![Some(String::new()), None],
    };
    assert_eq!(cvr.values.len(), cvp.paths.len());
    let wire = serde_json::to_string(&cvr).unwrap();
    assert_eq!(
        wire, r#"{"values":["",null]}"#,
        "a real empty cell (\"\") and an absent cell (null) are DISTINCT shapes"
    );
    let back: PluginColumnValuesResult = serde_json::from_str(&wire).unwrap();
    assert_eq!(back, cvr);
}

#[test]
fn rpc_cancel_params_roundtrip_num_and_str() {
    use norte_proto::methods::RpcCancelParams;
    use norte_proto::wire::RequestId;
    for id in [RequestId::Num(42), RequestId::Str("abc".into())] {
        let p = RpcCancelParams { id: id.clone() };
        let wire = serde_json::to_string(&p).expect("serializes");
        let back: RpcCancelParams = serde_json::from_str(&wire).expect("deserializes");
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

/// (0.30.0, ADR 0039) The FOUR new fields —spread across THREE surfaces:
/// catalogue, request ×2, and entry— are additive: an N-1 wire (0.29.x)
/// WITHOUT them deserializes, and an empty value is NOT emitted.
/// (`Entry.attrs`, the fourth, is covered by
/// `entry_with_valid_attributes_roundtrips_exactly` and the golden
/// `empty_attrs_are_omitted`.)
#[test]
fn attrs_are_additive_in_both_directions() {
    use norte_proto::methods::{FsCapabilitiesResult, FsListParams, FsStatParams};

    let n1 = r#"{"path":"file:///home","limit":null,"cursor":null}"#;
    let params: FsListParams = serde_json::from_str(n1).expect("valid N-1 wire");
    assert!(params.attrs.is_empty(), "absent = none requested");

    let wire = serde_json::to_string(&params).unwrap();
    assert!(
        !wire.contains("attrs"),
        "empty is not emitted (byte-identical to 0.29): {wire}"
    );

    let stat: FsStatParams =
        serde_json::from_str(r#"{"path":"file:///home"}"#).expect("valid N-1 wire");
    assert!(stat.attrs.is_empty());
    assert!(!serde_json::to_string(&stat).unwrap().contains("attrs"));

    let caps_n1 = r#"{"capabilities":{"flags":"RENAME_ATOMIC","max_path":null}}"#;
    let caps: FsCapabilitiesResult = serde_json::from_str(caps_n1).expect("valid N-1 wire");
    assert!(caps.attrs.is_empty());
    assert!(!serde_json::to_string(&caps).unwrap().contains("attrs"));
}

/// (0.30.0, ADR 0039) The asymmetry is DELIBERATE and enforced: the
/// catalogue (RECEIVED data) filters on decoding; a request (SENT data)
/// does not — a malformed id survives so the daemon can answer it with
/// `-32602` in block 2, instead of it turning into "nothing was requested".
#[test]
fn a_request_does_not_filter_but_the_catalogue_does() {
    use norte_proto::methods::{FsCapabilitiesResult, FsListParams, FsStatParams};

    let hostile = r#"{"path":"file:///home","attrs":["MODE","../etc/passwd"]}"#;
    let list: FsListParams = serde_json::from_str(hostile).expect("the request decodes as is");
    assert_eq!(
        list.attrs,
        ["MODE", "../etc/passwd"],
        "nothing is discarded"
    );
    let stat: FsStatParams = serde_json::from_str(hostile).expect("same for fs.stat");
    assert_eq!(stat.attrs, ["MODE", "../etc/passwd"]);

    let catalogue = r#"{
        "capabilities": {"flags":"RENAME_ATOMIC","max_path":null},
        "attrs": [
            {"id":"MODE","label":"Mode","type":"uint","hint":"mode"},
            {"id":"posix.mode","label":"Mode","type":"uint","hint":"mode"}
        ]
    }"#;
    let caps: FsCapabilitiesResult =
        serde_json::from_str(catalogue).expect("a hostile catalogue does not break the response");
    let ids: Vec<&str> = caps.attrs.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["posix.mode"], "the malformed id cannot be requested");
}

/// (0.30.0) SAMPLE ids —plausible, not a vocabulary: ADR 0039 §4 explicitly
/// refuses a central registry and registers none of these, which a provider
/// may or may not publish— in well-formed shape, against the hostile shapes
/// that are NOT well-formed. What is pinned is the GRAMMAR, and that the
/// gate lives in the type and not in every caller.
#[test]
fn sample_ids_are_well_formed() {
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
        assert!(is_valid_attr_id(id), "{id} is a well-formed example");
    }
    for hostile in [
        "../etc/passwd",
        "posix.mode\u{202E}",
        "POSIX.MODE",
        "",
        // A segment that does not start with a letter (0.30.0): argv shape and float shape.
        "-x.y",
        "0.0",
    ] {
        assert!(!is_valid_attr_id(hostile), "{hostile:?} must be rejected");
    }
}

// ---------- fs.compare (0.39.0) ----------

/// A minimal [`CompareRow`] on which each test changes only its own bit.
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

/// An N+1 daemon that adds a criterion cannot break an N-1 frontend: the
/// unknown token falls into the forward-compat variant, does not error.
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
    // 0.42.0 (#152): the fifth fallback in this family. And degrading is not
    // enough — a transformation this binary cannot name also does not know
    // whether it is harmless, so the cautious default is checked here.
    let pt: PairTransform = serde_json::from_str("\"transliteration\"").expect("degrades");
    assert_eq!(pt, PairTransform::Unknown);
    assert!(!pt.names_one_text());
}

/// 0.42.0 (#170, #195): the bump's other two fields are REQUIRED, and that
/// is a decision, not an oversight — a default would be a made-up answer
/// about whether something can be undone, or about which root a failure's
/// path hangs from. What backs this is the N/N-1 window: a 0.41 shape never
/// reaches a 0.42 decoder because the handshake does not negotiate a client
/// with a minor GREATER than the server's.
///
/// This test pins that, if it arrived, it is REJECTED. `DestTrash` and
/// `SyncStepKind` do not derive `Default`, so a plain `#[serde(default)]`
/// would not compile — but a `#[serde(default = "...")]` with an explicit
/// function does, and that is exactly the change to watch for
/// (`protocol-guardian`, W4b MINOR-3).
#[test]
fn the_two_required_fields_of_0_42_reject_the_0_41_shape() {
    use norte_proto::methods::{SyncFailure, SyncReportResult};
    let report_0_41 = serde_json::json!({
        "done": 3, "failed": 0, "skipped": 0, "bytes": 4096,
        "failures": [], "batch_id": 12
    });
    assert!(
        serde_json::from_value::<SyncReportResult>(report_0_41).is_err(),
        "without `dest_trash` there is no report: inventing it would answer \
         \"it can be undone\" without knowing"
    );
    let failure_0_41 = serde_json::json!({"rel": "a.txt", "cause": "denied"});
    assert!(
        serde_json::from_value::<SyncFailure>(failure_0_41).is_err(),
        "without `kind` there is no failure row: its absence would leave the \
         anchor back to the guesswork #195 closes"
    );
}

/// 0.42.0 (#152): `paired_under` is OPTIONAL and is omitted, so the row a
/// 0.41 daemon used to send keeps deserializing and an ordinary row still
/// travels exactly as it did before the bump.
#[test]
fn paired_under_is_additive_in_both_directions() {
    use norte_proto::methods::{CompareVerdict, PairTransform};
    let mut row = compare_row(
        CompareVerdict::Same,
        Some(compare_entry("file:///l/a")),
        Some(compare_entry("file:///r/a")),
    );
    let json = serde_json::to_value(&row).expect("json");
    assert!(
        json.as_object()
            .expect("object")
            .get("paired_under")
            .is_none(),
        "without a transformation there is no key: {json}"
    );
    // And the 0.41.0 shape —without the key— keeps deserializing to `None`.
    let back: norte_proto::methods::CompareRow = serde_json::from_value(json).expect("0.41 shape");
    assert_eq!(back, row);

    row.paired_under = Some(PairTransform::NormalizationSingleton);
    let json = serde_json::to_value(&row).expect("json");
    assert_eq!(
        json["paired_under"],
        serde_json::json!("normalization_singleton")
    );
}

/// `Unknown` in CONFIDENCE is a VALUE — "the provider cannot say" —, so that
/// enum's forward-compat fallback had to be named something else. Losing the
/// distinction would turn an honest answer into a protocol mismatch.
#[test]
fn confidence_unknown_is_a_value_not_the_fallback() {
    use norte_proto::methods::CompareConfidence;
    let known: CompareConfidence = serde_json::from_str("\"unknown\"").expect("a real value");
    assert_eq!(known, CompareConfidence::Unknown);
    let newer: CompareConfidence = serde_json::from_str("\"quantum\"").expect("degrades");
    assert_eq!(newer, CompareConfidence::Unrecognised);
    assert_ne!(known, newer);
}

/// The invariant the wire cannot express: the verdict determines which sides
/// are present. A row that says `OnlyLeft` while carrying a right entry is a
/// bug in whoever produced it, and this is where it gets caught.
#[test]
fn verdict_determines_which_sides_are_present() {
    use norte_proto::methods::CompareVerdict;
    let a = || compare_entry("file:///a");
    let b = || compare_entry("file:///b");
    assert!(compare_row(CompareVerdict::OnlyLeft, Some(a()), None).sides_are_consistent());
    assert!(!compare_row(CompareVerdict::OnlyLeft, Some(a()), Some(b())).sides_are_consistent());
    assert!(compare_row(CompareVerdict::Same, Some(a()), Some(b())).sides_are_consistent());
    assert!(!compare_row(CompareVerdict::Same, Some(a()), None).sides_are_consistent());
    // The rest of the vocabulary, by symmetry with the one above.
    assert!(compare_row(CompareVerdict::OnlyRight, None, Some(b())).sides_are_consistent());
    assert!(!compare_row(CompareVerdict::OnlyRight, Some(a()), None).sides_are_consistent());
    assert!(compare_row(CompareVerdict::Different, Some(a()), Some(b())).sides_are_consistent());
    assert!(!compare_row(CompareVerdict::Different, None, None).sides_are_consistent());
    assert!(compare_row(CompareVerdict::TypeMismatch, Some(a()), Some(b())).sides_are_consistent());
    assert!(!compare_row(CompareVerdict::TypeMismatch, Some(a()), None).sides_are_consistent());
}

/// The THREE verdicts with no side rule —the two that describe a problem
/// and the fallback— cannot fabricate one: `Ambiguous` names a collision on
/// ONE side, an `Error` from listing may have no entry to show, and nothing
/// is known about a verdict this client does not recognize. Claiming
/// otherwise would make an N-1 client distrust legitimate rows from an N+1
/// daemon.
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

/// `reason` answers "why" for exactly the two verdicts that have a why.
/// Anywhere else it is noise a client would have to guess at.
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
        // The fallback stays EXEMPT: an N+1 verdict may carry a reason, and
        // an N-1 client cannot know whether it applies.
        (CompareVerdict::Unknown, Some(CompareReason::CaseFold), true),
        (CompareVerdict::Unknown, None, true),
    ] {
        let mut row = compare_row(verdict, None, None);
        row.reason = reason;
        assert_eq!(row.reason_is_consistent(), ok, "{verdict:?} + {reason:?}");
    }
}

/// Round-trip of the whole row and of the params, with absent things ABSENT
/// from the wire (not `null`): the row travels millions of times through
/// `compare.rows`.
#[test]
fn compare_row_roundtrip_and_omissions() {
    use norte_proto::methods::{CompareRow, CompareVerdict, Side};
    let mut row = compare_row(
        CompareVerdict::OnlyLeft,
        Some(compare_entry("file:///a")),
        None,
    );
    assert_eq!(roundtrip(&row), row);
    let json = serde_json::to_string(&row).expect("json");
    for absent in ["right", "newer", "reason", "side"] {
        assert!(!json.contains(absent), "{absent} must not travel: {json}");
    }
    row.newer = Some(Side::Right);
    assert!(serde_json::to_string(&row).expect("json").contains("right"));
    // Unknown fields from an N+1 peer do not break the row.
    let future = r#"{"id":9,"verdict":"same","criterion":"size","confidence":"certain",
                     "future_field":true}"#;
    let row: CompareRow = serde_json::from_str(future).expect("tolerant");
    assert_eq!(row.id, 9);
    assert!(row.left.is_none() && row.right.is_none());
}

/// The criteria: `size` and `mtime` set, `hash` NOT, and a PARTIAL object
/// fills in from that default instead of failing. It is what decides
/// whether a comparison reads content, so the default matters as much as
/// the type.
#[test]
fn compare_criteria_default_and_partial() {
    use norte_proto::methods::{CompareCriteria, FsCompareParams};
    let d = CompareCriteria::default();
    assert!(d.size && d.mtime && !d.hash);
    let partial: CompareCriteria = serde_json::from_str(r#"{"hash":true}"#).expect("partial");
    assert!(partial.size && partial.mtime && partial.hash);

    let minimal = r#"{"left":"file:///a","right":"file:///b"}"#;
    let p: FsCompareParams = serde_json::from_str(minimal).expect("minimal params");
    assert_eq!(p.criteria, CompareCriteria::default());
    assert_eq!(p.mtime_tolerance_ms, 2000, "the FAT rule, by default");
    assert!(p.max_depth.is_none() && !p.follow_symlinks && p.descend_orphans.is_none());
    assert_eq!(roundtrip(&p), p);
}

/// `descend_orphans` accepts a side, is OMITTED when not requested —the
/// request from a client that does not know it is still byte-for-byte the
/// 0.39.0 one— and a TYPO dies in the deserializer.
///
/// That last part is the point: if the field were a [`Side`] (which
/// degrades with `serde(other)`), an `"lft"` would arrive as
/// `Some(Side::Unknown)` —no side— and the comparison would not descend
/// through any of them, silently serving a different set of rows than
/// requested. With [`DescendSide`] it is rejected by ANY peer's
/// deserializer, which is stronger than a check a handler could forget
/// (and the embedded arm, which goes through no handler at all, is covered
/// the same way).
#[test]
fn descend_orphans_is_omitted_when_not_requested_and_a_typo_does_not_degrade() {
    use norte_proto::methods::{DescendSide, FsCompareParams};
    let minimal = r#"{"left":"file:///a","right":"file:///b"}"#;
    let p: FsCompareParams = serde_json::from_str(minimal).expect("minimal params");
    let json = serde_json::to_value(&p).expect("json");
    assert!(
        json.get("descend_orphans").is_none(),
        "an absent field cannot appear on the wire: {json}"
    );

    let requested = r#"{"left":"file:///a","right":"file:///b","descend_orphans":"right"}"#;
    let p: FsCompareParams = serde_json::from_str(requested).expect("params");
    assert_eq!(p.descend_orphans, Some(DescendSide::Right));
    assert_eq!(roundtrip(&p), p);

    for bad in [r#""lft""#, r#""unknown""#, r#""both""#] {
        let raw = format!(r#"{{"left":"file:///a","right":"file:///b","descend_orphans":{bad}}}"#);
        assert!(
            serde_json::from_str::<FsCompareParams>(&raw).is_err(),
            "{bad} must not slip through as \"no side\""
        );
    }
}

/// [`FsCompareParams`] and [`SyncCompareOptions`] are THE SAME set of
/// comparison options with two wrappers: a method's and the one a plan
/// embeds. Nothing in the compiler ties them together —they are two
/// structs, and stay that way (0.40.0 cannot change `FsCompareParams`'s
/// already-published shape with a `flatten`)—, so a rung added to only one
/// would go unnoticed until a plan compared differently from `fs.compare`
/// over the same two trees.
///
/// The test ties them: same field NAMES and same VALUES for the same
/// configuration, minus the two roots, which in a plan are called `source`
/// and `dest` and live in `SyncPlanParams`.
#[test]
fn the_two_faces_of_the_compare_options_do_not_diverge() {
    use norte_proto::methods::{CompareCriteria, DescendSide, FsCompareParams, SyncCompareOptions};
    // Everything POPULATED: `Option`s are omitted when serializing, so a
    // field at `None` here would be a field this test does not look at.
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
    let embedded = SyncCompareOptions {
        criteria,
        max_depth: Some(3),
        mtime_tolerance_ms: 0,
        follow_symlinks: true,
        descend_orphans: Some(DescendSide::Left),
    };

    let mut from_method = serde_json::to_value(&params).expect("json");
    let object = from_method
        .as_object_mut()
        .expect("the params are an object");
    assert!(object.remove("left").is_some() && object.remove("right").is_some());
    assert_eq!(
        from_method,
        serde_json::to_value(&embedded).expect("json"),
        "a method's compare options and a plan's have diverged"
    );

    // And the DEFAULTS, which is where they would diverge without the names
    // moving: `FsCompareParams` takes them field by field
    // (`serde(default …)`) and `SyncCompareOptions` from a hand-written
    // `Default`. If they separate, a plan with `"compare": {}` would compare
    // differently from an `fs.compare` with no options, with the two types
    // identical in shape.
    let minimal: FsCompareParams =
        serde_json::from_str(r#"{"left":"file:///a","right":"file:///b"}"#).expect("params");
    let mut by_default = serde_json::to_value(&minimal).expect("json");
    let object = by_default.as_object_mut().expect("object");
    assert!(object.remove("left").is_some() && object.remove("right").is_some());
    assert_eq!(
        by_default,
        serde_json::to_value(SyncCompareOptions::default()).expect("json"),
        "the defaults of the two faces have diverged"
    );
}

// ---------- sync.plan (0.40.0) ----------

/// A minimal [`SyncStep`] on which each test changes only its own bit.
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

/// `reversal` is `None` if and ONLY IF the step is a `Skip`: a step that
/// does nothing has nothing to revert, and one that acts must say how it
/// comes back.
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

/// `reason` travels for EXACTLY two step shapes: the `Skip` and the one
/// that declares it cannot be undone. In any other it is noise.
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

/// An N+1 daemon that adds a step class cannot kill a batch of 256 in an
/// N-1 client: the unknown token degrades, as in the `compare.rows` family
/// (ADR 0048).
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
        "a step this client cannot judge is not declared inconsistent"
    );
}

/// The destination's trash goes daemon→client, so it degrades — and what
/// degrades promises nothing: `restores()` is `false` for the unknown
/// value, which is the safe direction (a dialog that does not know whether
/// something comes back cannot say it comes back).
#[test]
fn an_unknown_dest_trash_degrades_and_promises_nothing() {
    use norte_proto::methods::DestTrash;
    let t: DestTrash = serde_json::from_value(serde_json::json!("quantum")).expect("degrades");
    assert_eq!(t, DestTrash::Unknown);
    assert!(!t.restores());
    // And the three real answers, with the only one that returns something else.
    assert!(DestTrash::Restorable.restores());
    assert!(!DestTrash::Opaque.restores());
    assert!(!DestTrash::Absent.restores());
}

/// A plan's closure WITHOUT `dest_trash` does not decode, and that is
/// deliberate: a default would be making up whether something can be
/// undone. The same decision as `SyncCounts`'s new counters in the spool,
/// pinned here so removing it costs deleting a test.
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
        "without a declared trash there is no plan to approve"
    );
    v["dest_trash"] = serde_json::json!("absent");
    serde_json::from_value::<SyncPlanDone>(v).expect("with it, yes");
}

/// Client→daemon: accepting an unknown mode by default is accepting
/// deleting by default. It dies in the DESERIALIZER, which is stronger than
/// any check a handler could forget.
#[test]
fn a_mode_this_daemon_does_not_know_is_refused_not_defaulted() {
    use norte_proto::methods::{OnUnknown, SyncMode};
    assert!(serde_json::from_value::<SyncMode>(serde_json::json!("obliterate")).is_err());
    assert!(serde_json::from_value::<OnUnknown>(serde_json::json!("maybe")).is_err());
}

/// `type_mismatch_dir` is the only blocker whose side is not deduced from
/// its class, and at the same time the only one where the side IS the
/// sentence that gets shown. Without it there is nothing to display, so the
/// invariant is stated (and it is NOT rejected on deserialization: a
/// malformed blocker degrades, it does not kill the list).
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
    // The deserializer does NOT reject it: it arrives, and whoever reads it decides.
    let raw = serde_json::json!({"rel": "build", "kind": "type_mismatch_dir"});
    let arrived: SyncBlocker = serde_json::from_value(raw).expect("degrades, does not die");
    assert!(!arrived.shape_is_consistent());

    // The others owe no side: theirs is deduced from the class, or there is none.
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

/// Daemon→client: the blocker vocabulary round-trips whole.
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
    // ...and the `#[serde(other)]` fallback is still there, as in compare.
    let future: SyncBlockerKind = serde_json::from_str("\"cosmic_ray\"").expect("degrades");
    assert_eq!(future, SyncBlockerKind::Unknown);
}

/// `rel` is RELATIVE and the TYPE guarantees it: whatever would escape the
/// root never comes to exist, and dies in any peer's deserializer — not in
/// a daemon check that could be forgotten. A `VPath` could not promise this
/// without inventing a scheme (see `RelPath`'s rustdoc).
#[test]
fn a_rel_that_would_escape_its_root_dies_in_the_wire() {
    use norte_proto::methods::{RelPath, SyncStep};
    for hostile in ["..", "a/../b", "%2E%2E/etc", "/a", "a/", "a//b", ""] {
        let json = format!(
            r#"{{"id":1,"kind":"copy","rel":"{hostile}","criterion":"presence",
                 "confidence":"certain","reversal":"delete"}}"#
        );
        let step = serde_json::from_str::<SyncStep>(&json);
        // The empty string is the ROOT: legal, and the only case in the list.
        assert_eq!(
            step.is_ok(),
            hostile.is_empty(),
            "{hostile:?} must not cross the wire as a relative path"
        );
    }
    assert!(RelPath::default().is_root());
    // And the bytes come back intact (hard rule 1), segment by segment.
    let r = RelPath::parse_wire("sub/informe%FF%FE.dat").expect("rel");
    assert_eq!(r.segments()[1].as_bytes(), b"informe\xff\xfe.dat");
    assert_eq!(r.to_wire(), "sub/informe%FF%FE.dat");
}

/// `RelPath::under` is the ONLY way to measure a path against a root, so its
/// negatives are pinned here, in the crate that publishes it.
///
/// The three that matter are negative, and for the same reason: fail
/// closed. An extra `Some` would be a made-up relative path — and from
/// there comes an `include` that selects what the reader did not mark, or a
/// step that writes where nobody looked.
#[test]
fn under_measures_by_segments_and_fails_closed() {
    use norte_proto::VPath;
    use norte_proto::methods::RelPath;

    let root = VPath::parse("file:///origen").expect("root");
    assert_eq!(
        RelPath::under(&root, &VPath::parse("file:///origen/sub/a.txt").expect("p"))
            .expect("hangs from it")
            .to_wire(),
        "sub/a.txt"
    );
    // By SEGMENTS, not by string prefix: a sibling whose name STARTS with
    // the root's does not hang from it.
    assert!(RelPath::under(&root, &VPath::parse("file:///origen2/a.txt").expect("p")).is_none());
    // Shorter than the root.
    assert!(RelPath::under(&root, &VPath::parse("file:///").expect("p")).is_none());
    // A different scheme, and a different authority — byte for byte, with no
    // folding: for `mem://` and for an object-storage connection id the
    // authority is an opaque token, and folding it would merge two distinct
    // connections.
    assert!(RelPath::under(&root, &VPath::parse("mem:///origen/a.txt").expect("p")).is_none());
    let nas = VPath::parse("sftp://nas/d").expect("nas");
    assert!(RelPath::under(&nas, &VPath::parse("sftp://NAS/d/a.txt").expect("p")).is_none());
    // And the root itself comes out as the ROOT: it is the CORRECT value for
    // a `SyncBlocker` that talks about the whole tree and the most
    // destructive of a step, so deciding which of the two it is falls to
    // the caller.
    assert!(
        RelPath::under(&root, &root)
            .expect("the root hangs from itself")
            .is_root()
    );
    // The bytes are not touched along the way (hard rule 1).
    let hostile = VPath::parse("file:///origen/informe%FF%FE.dat").expect("p");
    assert_eq!(
        RelPath::under(&root, &hostile)
            .expect("hangs from it")
            .segments()[0]
            .as_bytes(),
        b"informe\xff\xfe.dat"
    );
}

/// `dest_rel` names the DESTINATION entry when its bytes are not the
/// source's, and it travels with the same codec as `rel`: two names a human
/// paints the same —NFC versus NFD— are two distinct byte sequences, and
/// the step distinguishes them.
///
/// The NFC/NFD half of the case is pinned HERE and not in `sync_step.json`,
/// on purpose: both forms are valid UTF-8, the segment codec leaves valid
/// UTF-8 literal, and a fixture with the two strings would read as two
/// IDENTICAL strings — an invisible failure in review, and vulnerable to
/// any editor's normalization silently stopping the check. Here the bytes
/// travel as escapes, which are ASCII and survive that. The other half —the
/// case, which IS visible— is what the golden freezes.
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
        "two distinct spellings are exactly the case the field covers"
    );
    assert_eq!(roundtrip(&s), s);
    // Both forms are VALID UTF-8, so the codec leaves them literal and the
    // two JSON strings paint the same. What distinguishes them are the
    // bytes, which is also the only thing the executor will open (hard rule 1).
    let json = serde_json::to_value(&s).expect("json");
    assert_ne!(json["rel"], json["dest_rel"]);
    let back: SyncStep = serde_json::from_value(json).expect("json");
    assert_eq!(
        back.rel.segments()[0].as_bytes(),
        "caf\u{e9}".as_bytes(),
        "5 bytes, one of them made of two"
    );
    assert_eq!(
        back.dest_rel.expect("dest_rel").segments()[0].as_bytes(),
        "cafe\u{301}".as_bytes(),
        "6 bytes: the e and its combining accent"
    );
}

/// The field's normative rule, checkable: `Some` ONLY when it differs. A
/// `dest_rel` equal to `rel` is not dangerous, it is noise — and a consumer
/// that sees it knows the producer did not apply the rule.
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
    // And it does not die on deserialization, like the rest of the shape: a
    // bad step degrades, it never kills the batch of 256.
    let json = serde_json::to_value(&s).expect("json");
    let back: SyncStep = serde_json::from_value(json).expect("degrades, does not die");
    assert_eq!(
        back.dest_rel,
        Some(RelPath::parse_wire("sub/b.txt").expect("rel"))
    );
    assert!(!back.shape_is_consistent());
}

/// Absent things are OMITTED from the wire (neither `null` nor the key): a
/// half-million-step plan sends `sync.steps` thousands of times.
#[test]
fn sync_step_roundtrip_and_omissions() {
    use norte_proto::methods::{StepReversal, SyncReason, SyncStep, SyncStepKind};
    let mut s = sync_step(SyncStepKind::Copy, Some(StepReversal::Delete), None);
    s.size = None;
    assert_eq!(roundtrip(&s), s);
    let json = serde_json::to_value(&s).expect("json");
    for absent in ["size", "reason", "dest_rel"] {
        assert!(
            json.get(absent).is_none(),
            "{absent} must not travel: {json}"
        );
    }
    // Unknown fields from an N+1 peer do not break the step.
    let future = r#"{"id":9,"kind":"skip","rel":"a","criterion":"presence",
                     "confidence":"unknown","reason":"unreadable","future_field":true}"#;
    let s: SyncStep = serde_json::from_str(future).expect("tolerant");
    assert_eq!(s.reason, Some(SyncReason::Unreadable));
    assert!(s.reversal.is_none() && s.size.is_none());
}

// ---------- SyncCounts::add (0.40.0) ----------

/// A step with the class, size, and reversal the test asks for.
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
        "it does not move them, so they are not unknown bytes either"
    );
}

/// A step that should not carry a size but does, does not contaminate the
/// total: the dialog shows bytes that are about to be WRITTEN, and a delete
/// does not write.
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
    // An orphan is not hydrated (#157) and `file://` lists without a size,
    // so this is the NORMAL case and not the rare one. A trusted zero in
    // the approval dialog would be a lie.
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

/// A step from an N+1 daemon does not enter any KNOWN class's counter —that
/// would lie about what the plan does— but it is still counted, because not
/// counting it would lie about how much plan there is.
#[test]
fn an_unknown_kind_is_counted_as_unknown_and_not_dropped() {
    use norte_proto::methods::{StepReversal, SyncCounts, SyncReason, SyncStep, SyncStepKind};
    let s: SyncStep = serde_json::from_value(serde_json::json!({
        "id": 3, "kind": "teleport", "rel": "a", "size": 99,
        "criterion": "size", "confidence": "certain",
        "reversal": "irreversible", "reason": "no_trash_on_target",
    }))
    .expect("degrades");
    assert_eq!(s.kind, SyncStepKind::Unknown);
    assert_eq!(s.reversal, Some(StepReversal::Irreversible));
    assert_eq!(s.reason, Some(SyncReason::NoTrashOnTarget));
    let mut c = SyncCounts::default();
    c.add(&s);
    assert_eq!(c.unknown_kind, 1);
    assert_eq!(
        c.irreversible, 1,
        "not knowing what it does does not make it reversible"
    );
    assert_eq!(
        (c.copy, c.overwrite, c.create_dir, c.delete_tree, c.skip),
        (0, 0, 0, 0, 0)
    );
    assert_eq!(
        (c.bytes, c.unmeasured_steps),
        (0, 0),
        "it is not known what it writes, so no bytes are attributed to it"
    );
}

/// `exact_bytes` is the `Option<u64>` that `bytes` is not: the total, or nothing.
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
        "with an unmeasured step there is no exact total to give"
    );
    assert_eq!(c.bytes, 10, "and the lower bound is still there");
    assert_eq!(SyncCounts::default().exact_bytes(), Some(0));
}

/// The invariant a consumer can check before trusting counters it did not
/// compute itself: only copy and overwrite move bytes.
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

/// Adding cannot kill the Task that is planning: it saturates.
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

/// L2: the session travels as an OPAQUE document. The round trip preserves
/// the whole body —including a kind no binary declares— because nobody
/// interprets it along the way.
#[test]
fn session_round_trips_an_opaque_body() {
    use norte_proto::methods::Session;
    let body = serde_json::json!({
        "version": 1,
        "layouts": { "default": { "kind": "kind-nobody-declares", "params": { "x": 1 } } },
        "slots": {}
    });
    let s = Session {
        version: 1,
        revision: 7,
        body: body.clone(),
    };
    let out = serde_json::to_string(&s).expect("serializes");
    let back: Session = serde_json::from_str(&out).expect("deserializes");
    assert_eq!(back.revision, 7);
    assert_eq!(back.body, body, "the body comes back whole, unnormalized");
}

/// The body's cap is the PROTOCOL's, not a loose constant of the daemon: the
/// client needs the same number to decide what to drop before retrying.
#[test]
fn session_body_max_is_one_mebibyte() {
    assert_eq!(norte_proto::methods::SESSION_BODY_MAX, 1024 * 1024);
}

/// `owner` travels on GET: the second client receives a copy and needs to
/// know it is one before trying to write.
#[test]
fn session_get_result_says_who_the_owner_is() {
    use norte_proto::methods::{Session, SessionGetResult};
    let r = SessionGetResult {
        session: Session {
            version: 1,
            revision: 0,
            body: serde_json::json!({}),
        },
        owner: false,
    };
    let v = serde_json::to_value(&r).expect("serializes");
    assert_eq!(v["owner"], serde_json::json!(false));
}

/// A session never written is the Default: revision 0 and no schema. The
/// core's store builds it that way, and the client distinguishes "there is
/// none" from "it failed".
#[test]
fn session_default_is_revision_zero() {
    let s = norte_proto::methods::Session::default();
    assert_eq!(s.revision, 0);
    assert_eq!(s.version, 0);
}
