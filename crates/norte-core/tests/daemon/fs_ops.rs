use super::*;

#[tokio::test]
async fn fs_stat_returns_only_what_was_asked_and_advertised() {
    let d = spawn_daemon_attrs().await;
    write_file(&d.mem, "mem:///f.txt", b"hola").await;
    let c = connected_client(&d).await;
    let r: FsStatResult = c
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///f.txt"),
                attrs: vec!["mem.mode".into(), "zz.unknown".into()],
            },
        )
        .await
        .expect("fs.stat");
    assert!(
        matches!(
            r.entry.attrs.get("mem.mode"),
            Some(norte_proto::AttrValue::Uint(_))
        ),
        "mem.mode materialized: {:?}",
        r.entry.attrs
    );
    // Valid but unadvertised id: ABSENT, never an error.
    assert!(!r.entry.attrs.contains_key("zz.unknown"));
    assert_eq!(r.entry.attrs.len(), 1);
}

#[tokio::test]
async fn call_tracked_reports_the_assigned_id() {
    let d = spawn_daemon(None).await;
    let client = connected_client(&d).await;
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
    let s = std::sync::Arc::clone(&seen);
    // fs.stat of a nonexistent path: the outcome (Err) does not matter, what
    // is checked is that on_id was invoked exactly once with an id > 0.
    let _res: Result<norte_proto::methods::FsStatResult, _> = client
        .call_tracked(
            norte_proto::methods::FS_STAT,
            &norte_proto::methods::FsStatParams {
                path: vp("mem:///nope"),
                attrs: Vec::new(),
            },
            move |id| s.lock().expect("lock").push(id),
        )
        .await;
    let seen = seen.lock().expect("lock");
    assert_eq!(seen.len(), 1, "on_id is invoked exactly once");
    assert!(seen[0] > 0, "assigned id > 0");
}

// ---------- fs.list pagination (ADR 0017) ----------

/// A page of `fs.list` with `limit`/`cursor`.
pub(super) async fn list_page(
    c: &Client,
    path: &str,
    limit: Option<u32>,
    cursor: Option<String>,
) -> FsListResult {
    c.call(
        methods::FS_LIST,
        &FsListParams {
            path: vp(path),
            limit,
            cursor,
            attrs: Vec::new(),
        },
    )
    .await
    .expect("fs.list")
}

pub(super) async fn seed(mem: &MemProvider, n: usize) {
    for i in 0..n {
        write_file(mem, &format!("mem:///f{i:03}.txt"), b"x").await;
    }
}

/// Paginating by cursor returns EXACTLY the same entries as the complete
/// listing, with no duplicates and none lost.
#[tokio::test]
async fn fs_list_paginated_concatenates_the_same_as_complete() {
    let d = spawn_daemon(None).await;
    seed(&d.mem, 5).await;
    let c = connected_client(&d).await;

    let complete = list_page(&c, "mem:///", None, None).await;
    assert_eq!(complete.entries.len(), 5);
    assert!(
        complete.next_cursor.is_none(),
        "no cursor = complete listing"
    );

    // Pages of 2.
    let mut accumulated = Vec::new();
    let mut cursor = None;
    loop {
        let page = list_page(&c, "mem:///", Some(2), cursor).await;
        assert!(page.entries.len() <= 2, "respects the limit");
        accumulated.extend(page.entries);
        match page.next_cursor {
            Some(cur) => cursor = Some(cur),
            None => break,
        }
    }
    // Same set of paths (the provider's order can vary).
    let mut a: Vec<_> = accumulated.iter().map(|e| e.path.clone()).collect();
    let mut b: Vec<_> = complete.entries.iter().map(|e| e.path.clone()).collect();
    a.sort();
    b.sort();
    assert_eq!(a, b, "paginated == complete");
}

/// #93: `skipped` (omitted from the container) travels in `fs.list`'s result
/// and is REPEATED on every page (the client can latch onto any of them).
/// With a normal provider (nothing omitted) the field comes back absent
/// (`None`).
#[tokio::test]
async fn fs_list_skipped_travels_on_every_page() {
    // A daemon with a MemProvider that simulates a container with 7 omitted.
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::new().with_list_skipped(7));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let daemon = Daemon::bind(
        engine,
        DaemonConfig {
            socket_path: Some(socket.clone()),
            idle_timeout: None,
            listing_ttl: Duration::from_mins(2),
            plugins_dir: Some(dir.path().to_path_buf()),
            state_dir: None,
        },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    let d = TestDaemon {
        socket,
        run,
        dir,
        mem,
    };
    seed(&d.mem, 5).await;
    let c = connected_client(&d).await;

    // Complete listing (no cursor): it carries it.
    let complete = list_page(&c, "mem:///", None, None).await;
    assert_eq!(complete.skipped, Some(7));

    // Paginated: EVERY page repeats it (first, middle and last).
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let page = list_page(&c, "mem:///", Some(2), cursor).await;
        assert_eq!(page.skipped, Some(7), "page {pages}");
        pages += 1;
        match page.next_cursor {
            Some(cur) => cursor = Some(cur),
            None => break,
        }
    }
    assert!(pages >= 3, "there really were continuations");

    // Provider with nothing omitted (normal spawn): the field comes back
    // absent.
    let d2 = spawn_daemon(None).await;
    seed(&d2.mem, 1).await;
    let c2 = connected_client(&d2).await;
    assert_eq!(list_page(&c2, "mem:///", None, None).await.skipped, None);
}

#[tokio::test]
async fn fs_stat_of_nonexistent_travels_as_taxonomy_in_data() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, FsStatResult>(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///nada"),
                attrs: Vec::new(),
            },
        )
        .await
        .expect_err("does not exist");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert_eq!(rpc.data, Some(norte_proto::Error::NotFound));
        }
        other => panic!("expected Rpc with data, got {other:?}"),
    }
}

/// H1 (encoding review #108-b2): HOSTILE values cross the real socket —
/// non-UTF-8 `Bytes`, byte-exact after `encode(bytes_b64)+decode`, and `Text`
/// with an RTL override/ZWJ, char-exact after the emission belt.
#[tokio::test]
async fn hostile_attrs_cross_the_socket_byte_exact() {
    let d = spawn_daemon_attrs().await;
    write_file(&d.mem, "mem:///f.txt", b"x").await;
    let c = connected_client(&d).await;
    let r: FsStatResult = c
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///f.txt"),
                attrs: vec!["mem.owner".into(), "mem.note".into()],
            },
        )
        .await
        .expect("fs.stat");
    assert_eq!(
        r.entry.attrs.get("mem.owner"),
        Some(&norte_proto::AttrValue::Bytes(
            b"due\xf1o-\xff\xfe".to_vec()
        )),
        "raw bytes byte-exact after the wire"
    );
    assert_eq!(
        r.entry.attrs.get("mem.note"),
        Some(&norte_proto::AttrValue::Text(
            "\u{202e}atón\u{202c} a\u{200d}b".to_owned()
        )),
        "hostile text char-exact after the wire"
    );
}
