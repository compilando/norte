use super::*;

// ---------- fs.* dispatch ----------

#[tokio::test]
async fn fs_list_and_stat_answer_over_the_socket() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///f.txt", b"hola").await;
    let c = connected_client(&d).await;

    let list: FsListResult = c
        .call(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: Vec::new(),
            },
        )
        .await
        .expect("fs.list");
    assert_eq!(list.entries.len(), 1);

    let stat: FsStatResult = c
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///f.txt"),
                attrs: Vec::new(),
            },
        )
        .await
        .expect("fs.stat");
    assert_eq!(stat.entry.size, Some(4));
}

// ---------- attrs over the wire (#108 block 2, ADR 0039) ----------
// (Replaces block 1's pin "the daemon ignores requested ids": from this
// block on, the daemon validates, crosses with what is advertised, and
// materializes.)

pub(super) fn assert_rpc_code(err: &ClientError, code: i64) {
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, code, "RPC code: {rpc:?}"),
        other => panic!("expected RPC error {code}, got {other:?}"),
    }
}

/// A daemon with a `MemProvider` of synthetic attrs: the catalogue arrives
/// via `fs.capabilities` (sanitized by `AttrCatalog::new`) and
/// `fs.list`/`fs.stat` materialize requested∩advertised.
pub(super) async fn spawn_daemon_attrs() -> TestDaemon {
    spawn_daemon_mem(
        None,
        Duration::from_mins(2),
        MemProvider::new().with_synthetic_attrs(),
    )
    .await
}

#[tokio::test]
async fn fs_capabilities_publishes_the_providers_catalogue() {
    let d = spawn_daemon_attrs().await;
    let c = connected_client(&d).await;
    let r: methods::FsCapabilitiesResult = c
        .call(
            methods::FS_CAPABILITIES,
            &methods::FsCapabilitiesParams {
                path: vp("mem:///"),
            },
        )
        .await
        .expect("fs.capabilities");
    assert!(
        r.attrs.iter().any(|a| a.id == "mem.owner"),
        "published catalogue: {:?}",
        r.attrs
    );
}

#[tokio::test]
async fn fs_list_malformed_or_over_cap_attrs_is_invalid_params() {
    let d = spawn_daemon_attrs().await;
    let c = connected_client(&d).await;
    // Malformed id (uppercase) → -32602.
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: vec!["MAYUS.no".into()],
            },
        )
        .await
        .expect_err("a malformed id must be an error");
    assert_rpc_code(&err, codes::INVALID_PARAMS);
    // 17 valid ids (the deserializer materializes 16+1 as a witness).
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: (0..17).map(|i| format!("a.b{i}")).collect(),
            },
        )
        .await
        .expect_err("over the cap must be an error");
    assert_rpc_code(&err, codes::INVALID_PARAMS);
}

#[tokio::test]
async fn fs_list_paginated_keeps_the_start_up_attrs() {
    let d = spawn_daemon_attrs().await;
    seed(&d.mem, 3).await;
    let c = connected_client(&d).await;
    // First page WITH attrs; continuations WITHOUT resending them.
    let mut r: FsListResult = c
        .call(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: Some(1),
                cursor: None,
                attrs: vec!["mem.mode".into()],
            },
        )
        .await
        .expect("fs.list");
    let mut total = 0;
    loop {
        // EVERY entry of EVERY page carries the start-up attr (the last page
        // can come back empty: the stream does not know it ended until it is
        // drained).
        for e in &r.entries {
            assert!(
                e.attrs.contains_key("mem.mode"),
                "entry with no mem.mode: {e:?}"
            );
        }
        total += r.entries.len();
        let Some(cursor) = r.next_cursor.clone() else {
            break;
        };
        // The continuation does not resend attrs: the retained stream
        // already carries them.
        r = list_page(&c, "mem:///", Some(1), Some(cursor)).await;
    }
    assert_eq!(total, 3);
}

/// `limit = 0` is a params error (avoids empty pages in a loop).
#[tokio::test]
async fn fs_list_limit_zero_is_invalid_params() {
    let d = spawn_daemon(None).await;
    seed(&d.mem, 2).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: Some(0),
                cursor: None,
                attrs: Vec::new(),
            },
        )
        .await
        .expect_err("limit 0");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// An unknown (or non-numeric) cursor → `CursorExpired`: the client restarts.
#[tokio::test]
async fn fs_list_unknown_cursor_is_cursor_expired() {
    let d = spawn_daemon(None).await;
    seed(&d.mem, 2).await;
    let c = connected_client(&d).await;
    for cur in ["999", "non-numeric"] {
        let err = c
            .call::<_, FsListResult>(
                methods::FS_LIST,
                &FsListParams {
                    path: vp("mem:///"),
                    limit: Some(1),
                    cursor: Some(cur.to_string()),
                    attrs: Vec::new(),
                },
            )
            .await
            .expect_err("invalid cursor");
        match err {
            ClientError::Rpc(rpc) => {
                assert_eq!(rpc.data, Some(norte_proto::Error::CursorExpired), "{cur}");
            }
            other => panic!("expected Rpc, got {other:?}"),
        }
    }
}

/// A cursor from ANOTHER path → `INVALID_PARAMS` (the cursor validates
/// against its dir).
#[tokio::test]
async fn fs_list_cursor_from_another_path_is_invalid_params() {
    let d = spawn_daemon(None).await;
    seed(&d.mem, 3).await;
    let c = connected_client(&d).await;
    // Opens a listing of the root that RETAINS (limit 1, there are 3
    // entries).
    let page = list_page(&c, "mem:///", Some(1), None).await;
    let cur = page.next_cursor.expect("retains");
    // Continue it with ANOTHER path: the path check goes BEFORE listing, so
    // the other path does not even need to exist.
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///otro"),
                limit: Some(1),
                cursor: Some(cur),
                attrs: Vec::new(),
            },
        )
        .await
        .expect_err("cursor from another path");
    assert!(matches!(err, ClientError::Rpc(rpc) if rpc.code == codes::INVALID_PARAMS));
}

/// LRU: opening the 9th retained listing evicts the oldest one (its cursor →
/// `CursorExpired`).
#[tokio::test]
async fn fs_list_lru_evicts_the_oldest() {
    let d = spawn_daemon(None).await;
    seed(&d.mem, 3).await; // ≥2 so each limit=1 retains
    let c = connected_client(&d).await;
    // Opens 9 listings (MAX_OPEN_LISTINGS = 8): the 9th evicts the 1st.
    let mut cursors = Vec::new();
    for _ in 0..9 {
        let page = list_page(&c, "mem:///", Some(1), None).await;
        cursors.push(page.next_cursor.expect("retains"));
    }
    // The first cursor was evicted.
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: Some(1),
                cursor: Some(cursors[0].clone()),
                attrs: Vec::new(),
            },
        )
        .await
        .expect_err("the 1st was evicted");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.data, Some(norte_proto::Error::CursorExpired));
        }
        other => panic!("expected Rpc, got {other:?}"),
    }
    // The last one is still alive.
    let ok = list_page(&c, "mem:///", Some(1), Some(cursors[8].clone())).await;
    assert!(!ok.entries.is_empty(), "the most recent one survives");
}

/// TTL: a retained listing that is not continued expires (its cursor →
/// `CursorExpired`).
#[tokio::test]
async fn fs_list_ttl_expires_the_listing() {
    let d = spawn_daemon_ttl(None, Duration::from_millis(150)).await;
    seed(&d.mem, 3).await;
    let c = connected_client(&d).await;
    let page = list_page(&c, "mem:///", Some(1), None).await;
    let cur = page.next_cursor.expect("retains");
    // A timer OF THE SYSTEM UNDER TEST, not a wait of ours: what this test
    // checks is that the 150ms TTL expires the listing, so that much time has
    // to pass. It cannot be polled — expiring is ceasing to be — nor skipped
    // with a virtual clock: the daemon runs on its own runtime and the test
    // does not control its timers.
    //
    // Making it deterministic would mean injecting the clock into the
    // daemon, which is a production change not worth it for one test. The
    // 400ms are 2.6× the span.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let err = c
        .call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: Some(1),
                cursor: Some(cur),
                attrs: Vec::new(),
            },
        )
        .await
        .expect_err("expired by TTL");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.data, Some(norte_proto::Error::CursorExpired));
        }
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// rust-reviewer B1: a `call()` AFTER the connection dies fails with
/// `ConnectionClosed` instead of hanging forever.
#[tokio::test]
async fn a_call_after_closing_does_not_hang() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    // Shutting down the daemon leaves the connection dead.
    let _: DaemonShutdownResult = c
        .call(
            methods::DAEMON_SHUTDOWN,
            &DaemonShutdownParams {
                graceful: true,
                ..Default::default()
            },
        )
        .await
        .expect("shutdown");
    tokio::time::timeout(Duration::from_secs(5), d.run)
        .await
        .expect("shut down")
        .expect("join")
        .expect("run ok");
    // No fixed margin: what this test asserts is that the call ANSWERS and
    // NEVER hangs, and the `timeout` below already backs that. Sleeping
    // first only made the interesting case — calling BEFORE the reader sees
    // the EOF — never get tested.
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        c.call::<_, FsListResult>(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: Vec::new(),
            },
        ),
    )
    .await
    .expect("answers, NEVER hangs")
    .expect_err("the connection is dead");
    assert!(
        matches!(err, ClientError::ConnectionClosed | ClientError::Io(_)),
        "{err:?}"
    );
}

/// `fs.capabilities`: the frontend decides (F8 trash, ADR 0009) with no logic
/// of its own.
#[tokio::test]
async fn fs_capabilities_travels_over_the_socket() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let r: methods::FsCapabilitiesResult = c
        .call(
            methods::FS_CAPABILITIES,
            &methods::FsCapabilitiesParams {
                path: vp("mem:///"),
            },
        )
        .await
        .expect("fs.capabilities");
    assert!(
        r.capabilities
            .flags
            .contains(norte_proto::CapabilityFlags::TRASH),
        "MemProvider declares TRASH: {:?}",
        r.capabilities.flags
    );
}

/// #53 (M2, rule 3 + DoD): closing a connection with retained listings
/// RELEASES them (RAII: `ConnState` drop → `OpenListing` drop → the guard
/// decrements the global cap and the producer dies). Observable end-to-end
/// through the global cap's degradation: saturated, a paginated `fs.list`
/// degrades to full-listing (`next_cursor=None`); freed, it paginates again.
#[tokio::test]
async fn closing_a_connection_frees_its_retained_listings() {
    let d = spawn_daemon(None).await;
    for i in 0..3u32 {
        write_file(&d.mem, &format!("mem:///f{i}.bin"), b"x").await;
    }
    let page = |c: &'static str| FsListParams {
        path: vp(c),
        limit: Some(1),
        cursor: None,
        attrs: Vec::new(),
    };

    // Saturates the GLOBAL cap (256): 32 connections × 8 retained listings.
    let mut hoarders = Vec::new();
    for _ in 0..32 {
        let c = connected_client(&d).await;
        for _ in 0..8 {
            let r: FsListResult = c
                .call(methods::FS_LIST, &page("mem:///"))
                .await
                .expect("fs.list");
            assert!(r.next_cursor.is_some(), "retained (still under the cap)");
        }
        hoarders.push(c);
    }
    // Saturated: a new page DEGRADES to full-listing (does not retain).
    let probe = connected_client(&d).await;
    let r: FsListResult = probe
        .call(methods::FS_LIST, &page("mem:///"))
        .await
        .expect("degraded fs.list");
    assert!(r.next_cursor.is_none(), "saturated degrades to complete");
    assert_eq!(r.entries.len(), 3, "degraded = the WHOLE listing");

    // ONE hoarding connection drops: its 8 listings must be released (RAII).
    drop(hoarders.pop());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let r: FsListResult = probe
            .call(methods::FS_LIST, &page("mem:///"))
            .await
            .expect("fs.list after freeing");
        if r.next_cursor.is_some() {
            break; // paginating again: the global cap went down — freed.
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the dead connection's listings were not freed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Empty criteria: `INVALID_PARAMS` with detail and WITHOUT creating a Task.
#[tokio::test]
async fn fs_search_invalid_params_create_no_task() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let params = FsSearchParams::new(vp("mem:///"));
    let err = c
        .call::<_, FsTaskResult>(methods::FS_SEARCH, &params)
        .await
        .expect_err("no criteria");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("expected Rpc INVALID_PARAMS, got {other:?}"),
    }
    // No live nor recent task: validation failed BEFORE the submit.
    let list: FsListResult = c
        .call(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: Vec::new(),
            },
        )
        .await
        .expect("fs.list");
    let _ = list; // (no seeded entries)
    let tasks: methods::TaskListResult = c
        .call(methods::TASK_LIST, &methods::TaskListParams::default())
        .await
        .expect("task.list");
    assert!(tasks.tasks.is_empty(), "no Task was created");
}

/// m2 (protocol-guardian #108-b2): block 1's guarantee still holds — asking
/// for a WELL-FORMED id from a provider with an EMPTY catalogue succeeds in
/// `fs.list` (not -32602) and the entries come back bare.
#[tokio::test]
async fn fs_list_valid_id_over_an_empty_catalogue_is_not_an_error() {
    let d = spawn_daemon(None).await; // MemProvider with NO synthetic attrs
    write_file(&d.mem, "mem:///f.txt", b"hola").await;
    let c = connected_client(&d).await;
    let list: FsListResult = c
        .call(
            methods::FS_LIST,
            &FsListParams {
                path: vp("mem:///"),
                limit: None,
                cursor: None,
                attrs: vec!["posix.mode".into()],
            },
        )
        .await
        .expect("a valid id over an empty catalogue is never an error");
    assert_eq!(list.entries.len(), 1);
    for e in &list.entries {
        assert!(e.attrs.is_empty(), "empty catalogue = bare entries");
    }
}
