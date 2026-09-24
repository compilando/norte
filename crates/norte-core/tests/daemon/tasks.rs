use super::*;

/// An `archive.test` report belongs to whoever launched the Task: another
/// actor's id is answered the same as one that never existed, which is what
/// its two twins do (`fs.rename_batch_report`, `sync.report`).
#[tokio::test]
async fn an_archive_tests_report_is_not_anyones() {
    let d = spawn_daemon(None).await;
    let human = connected_client(&d).await;
    let err = human
        .call::<_, methods::ArchiveTestResult>(
            methods::ARCHIVE_TEST_REPORT,
            &methods::ArchiveTestReportParams {
                task_id: norte_proto::TaskId::new(4242),
            },
        )
        .await
        .expect_err("that id was never a test");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(
                rpc.data,
                Some(norte_proto::Error::NotFound),
                "{:?}",
                rpc.data
            );
        }
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// #250 — an `archive.pack` report has the SAME discipline as an archive
/// test's: an id that was never an `archive.pack` answers `NotFound`, which
/// is also what is answered for another actor's.
///
/// Exists because the handler is a twin of the one next to it and "it's a
/// twin" is not evidence: what this pins down is that the method is WIRED
/// into the dispatch — renaming it or not routing it would pass the whole
/// suite — and that its answer to the unknown does not leak existence.
#[tokio::test]
async fn an_archive_packs_report_is_not_anyones() {
    let d = spawn_daemon(None).await;
    let human = connected_client(&d).await;
    let err = human
        .call::<_, methods::ArchivePackReportResult>(
            methods::ARCHIVE_PACK_REPORT,
            &methods::ArchivePackReportParams {
                task_id: norte_proto::TaskId::new(4242),
            },
        )
        .await
        .expect_err("that id was never a pack");
    match err {
        ClientError::Rpc(rpc) => {
            assert_eq!(
                rpc.data,
                Some(norte_proto::Error::NotFound),
                "{:?}",
                rpc.data
            );
        }
        other => panic!("expected Rpc, got {other:?}"),
    }
}

// ---------- tasks: progress, terminal, cancel, broadcast ----------

/// Waits for `task.progress` notifications of a task up to its terminal
/// state; returns the snapshots seen.
pub(super) async fn drain_task(c: &mut Client, task_id: u64) -> Vec<TaskProgress> {
    let mut seen = Vec::new();
    loop {
        let n = tokio::time::timeout(Duration::from_secs(5), c.notification())
            .await
            .expect("notification before the timeout")
            .expect("connection alive");
        assert_eq!(n.method, methods::TASK_PROGRESS);
        let p: TaskProgress =
            serde_json::from_value(n.params.expect("params")).expect("TaskProgress");
        if p.task_id.get() != task_id {
            continue;
        }
        let terminal = p.state.is_terminal();
        seen.push(p);
        if terminal {
            return seen;
        }
    }
}

#[tokio::test]
async fn fs_copy_progresses_to_completed() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///src.bin", &vec![0xAB; 5000]).await;
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///src.bin"),
                to: vp("mem:///dst.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect("fs.copy");
    let seen = drain_task(&mut c, task.task_id.get()).await;
    let last = seen.last().expect("at least the terminal");
    assert_eq!(last.state, TaskState::Completed);
    assert_eq!(last.bytes_done, 5000);
    let stat: FsStatResult = c
        .call(
            methods::FS_STAT,
            &FsStatParams {
                path: vp("mem:///dst.bin"),
                attrs: Vec::new(),
            },
        )
        .await
        .expect("the destination exists");
    assert_eq!(stat.entry.size, Some(5000));
}

#[tokio::test]
async fn task_cancel_over_the_socket_cancels_cleanly() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///grande.bin", &vec![0xCD; 100_000]).await;
    // Per-op latency: gives time to cancel mid-way.
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(30)));
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///grande.bin"),
                to: vp("mem:///copia.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect("fs.copy");
    let _: TaskCancelResult = c
        .call(
            methods::TASK_CANCEL,
            &TaskCancelParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("task.cancel");
    let seen = drain_task(&mut c, task.task_id.get()).await;
    assert_eq!(
        seen.last().expect("terminal").state,
        TaskState::Cancelled,
        "cooperative cancellation confirmed by notification"
    );
}

/// ADR 0147: `task.pause` catches the copy at its next chunk — it says so
/// with `paused`, and the destination does not exist under its name yet —
/// and `task.resume` lets it finish entirely.
#[tokio::test]
async fn task_pause_and_resume_over_the_socket() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///grande.bin", &vec![0xCD; 100_000]).await;
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(30)));
    let mut c = connected_client(&d).await;
    let task: FsTaskResult = c
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///grande.bin"),
                to: vp("mem:///copia.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect("fs.copy");
    let _: norte_proto::methods::TaskPauseResult = c
        .call(
            methods::TASK_PAUSE,
            &norte_proto::methods::TaskPauseParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("task.pause");
    loop {
        let n = tokio::time::timeout(Duration::from_secs(5), c.notification())
            .await
            .expect("notification before the timeout")
            .expect("connection alive");
        let p: TaskProgress =
            serde_json::from_value(n.params.expect("params")).expect("TaskProgress");
        if p.task_id != task.task_id {
            continue;
        }
        assert!(
            !p.state.is_terminal(),
            "finished without pausing: {:?}",
            p.state
        );
        if p.state == TaskState::Paused {
            break;
        }
    }
    let _: norte_proto::methods::TaskPauseResult = c
        .call(
            methods::TASK_RESUME,
            &norte_proto::methods::TaskPauseParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("task.resume");
    let seen = drain_task(&mut c, task.task_id.get()).await;
    let last = seen.last().expect("terminal");
    assert_eq!(last.state, TaskState::Completed);
    assert_eq!(last.bytes_done, 100_000);
}

/// The basis of phase 3: a SECOND client sees the progress of the tasks the
/// first one queued.
#[tokio::test]
async fn progress_is_broadcast_to_every_client() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///src.bin", &vec![0xEE; 2000]).await;
    let c1 = connected_client(&d).await;
    let mut c2 = connected_client(&d).await;

    let task: FsTaskResult = c1
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///src.bin"),
                to: vp("mem:///dst.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect("c1's fs.copy");
    // c2 asked for nothing — and still sees c1's task up to the terminal.
    let seen = drain_task(&mut c2, task.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);
}

// ---------- 0.5.0 methods (phase 3) ----------

/// `task.list` returns the snapshot of LIVE tasks: the resync of a frontend
/// that connects late.
#[tokio::test]
async fn task_list_gives_the_snapshot_of_live_tasks() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///grande.bin", &vec![0xAB; 50_000]).await;
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(20)));
    let c1 = connected_client(&d).await;
    let task: FsTaskResult = c1
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///grande.bin"),
                to: vp("mem:///copia.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect("fs.copy");
    // LATE client: sees the first one's task via task.list.
    let mut c2 = connected_client(&d).await;
    let list: methods::TaskListResult = c2
        .call(methods::TASK_LIST, &methods::TaskListParams {})
        .await
        .expect("task.list");
    assert!(
        list.tasks.iter().any(|t| t.task_id == task.task_id),
        "the other client's live task shows up: {list:?}"
    );
    // And keeps seeing it progress to the terminal via broadcast.
    let seen = drain_task(&mut c2, task.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);
}

/// `fs.read` with a range: exact bytes in base64 and an honest eof.
#[tokio::test]
async fn fs_read_returns_chunks_with_an_honest_eof() {
    use base64::Engine as _;
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///f.bin", b"0123456789").await;
    let c = connected_client(&d).await;

    let r: methods::FsReadResult = c
        .call(
            methods::FS_READ,
            &methods::FsReadParams {
                path: vp("mem:///f.bin"),
                range: Some(norte_proto::ByteRange {
                    offset: 2,
                    len: Some(3),
                }),
            },
        )
        .await
        .expect("fs.read");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&r.content_b64)
        .expect("valid base64");
    assert_eq!(bytes, b"234");
    assert!(!r.eof, "bytes remain after the chunk");

    // Chunk up to the end: eof true.
    let r: methods::FsReadResult = c
        .call(
            methods::FS_READ,
            &methods::FsReadParams {
                path: vp("mem:///f.bin"),
                range: Some(norte_proto::ByteRange {
                    offset: 5,
                    len: Some(100),
                }),
            },
        )
        .await
        .expect("fs.read");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&r.content_b64)
        .expect("valid base64");
    assert_eq!(bytes, b"56789");
    assert!(r.eof);

    // No range: the whole file (fits well within the cap).
    let r: methods::FsReadResult = c
        .call(
            methods::FS_READ,
            &methods::FsReadParams {
                path: vp("mem:///f.bin"),
                range: None,
            },
        )
        .await
        .expect("fs.read");
    assert!(r.eof);
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&r.content_b64)
            .expect("base64"),
        b"0123456789"
    );
}

/// An AGENT's `task.pause` over another actor's task (ADR 0147): the same ack
/// as an unknown one, and WITH NO effect — the human's copy never stops and
/// completes.
#[tokio::test]
async fn an_agents_pause_does_not_touch_the_humans_task() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///grande.bin", &vec![0xCD; 100_000]).await;
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(500)));
    let mut human = connected_client(&d).await;
    let agent = connected_agent(&d, "sess-pause").await;
    let task: FsTaskResult = human
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///grande.bin"),
                to: vp("mem:///copia.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect("the human's fs.copy");
    let other: norte_proto::methods::TaskPauseResult = agent
        .call(
            methods::TASK_PAUSE,
            &norte_proto::methods::TaskPauseParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("ack: does not leak other actors' task existence");
    let unknown: norte_proto::methods::TaskPauseResult = agent
        .call(
            methods::TASK_PAUSE,
            &norte_proto::methods::TaskPauseParams {
                task_id: norte_proto::TaskId::new(u64::MAX - 3),
            },
        )
        .await
        .expect("ack also for one that does not exist");
    assert_eq!(other, unknown, "another's and unknown, indistinguishable");
    let seen = drain_task(&mut human, task.task_id.get()).await;
    assert!(
        seen.iter().all(|p| p.state != TaskState::Paused),
        "an agent's pause over another actor's task has NO effect"
    );
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);
}

/// An AGENT's `task.cancel` over another actor's task: ack (the contract does
/// not leak existence) but WITH NO effect — the human's copy completes.
#[tokio::test]
async fn an_agents_cancel_does_not_touch_the_humans_task() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///grande.bin", &vec![0xCD; 100_000]).await;
    // GENEROUS per-op latency (the copy is ~4-6 ops, not proportional to
    // size): the task stays alive when the hostile cancel arrives even on a
    // loaded runner — if it finished earlier, the test would pass vacuously.
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(500)));
    let mut human = connected_client(&d).await;
    let agent = connected_agent(&d, "sess-cancel").await;

    let task: FsTaskResult = human
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///grande.bin"),
                to: vp("mem:///copia.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect("the human's fs.copy");
    let _: TaskCancelResult = agent
        .call(
            methods::TASK_CANCEL,
            &TaskCancelParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("ack: does not leak other actors' task existence");
    let seen = drain_task(&mut human, task.task_id.get()).await;
    assert_eq!(
        seen.last().expect("terminal").state,
        TaskState::Completed,
        "an agent's cancellation over another actor's task has NO effect"
    );
}

/// An agent DOES cancel its own task (the gate does not over-block).
#[tokio::test]
async fn an_agents_cancel_cancels_its_own() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///grande.bin", &vec![0xEF; 100_000]).await;
    // Generous latency: if the copy completed before the cancel, the
    // terminal would be Completed and the test would fail on timing, not on
    // the gate.
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(500)));
    let mut agent = connected_agent(&d, "sess-own").await;

    let task: FsTaskResult = agent
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///grande.bin"),
                to: vp("mem:///copia.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect("the agent's fs.copy");
    let _: TaskCancelResult = agent
        .call(
            methods::TASK_CANCEL,
            &TaskCancelParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("own task.cancel");
    let seen = drain_task(&mut agent, task.task_id.get()).await;
    assert_eq!(
        seen.last().expect("terminal").state,
        TaskState::Cancelled,
        "cancelling your own still works"
    );
}

/// An unknown (or ring-evicted) `task_id` is `NotFound` from the taxonomy
/// (0.79.0), the same as its twin `fs.rename_batch_report` and the same as
/// the embedded arm answers. It used to be a bare `INVALID_PARAMS`, which the
/// client read as `Internal` — the answer of a panicking provider.
#[tokio::test]
async fn undo_report_of_an_unknown_task_is_not_found() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::PolicyUndoReportResult>(
            methods::POLICY_UNDO_REPORT,
            &methods::PolicyUndoReportParams {
                task_id: norte_proto::TaskId::new(424_242),
            },
        )
        .await
        .expect_err("with no undo there is no report");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::NotFound)),
            "NotFound from the taxonomy, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// #72 (edge case): an `rpc.cancel` of an UNKNOWN id (nothing in flight) is a
/// benign no-op — it neither hangs nor breaks the connection. Also covers the
/// case of a stray `rpc.cancel` while NOTHING is suspended.
#[tokio::test]
async fn rpc_cancel_of_an_unknown_id_is_a_no_op() {
    let d = spawn_daemon_ask(Duration::from_secs(30)).await;
    let agent = connected_agent(&d, "s1").await;

    agent
        .notify(
            methods::RPC_CANCEL,
            &methods::RpcCancelParams {
                id: norte_proto::wire::RequestId::Num(9999),
            },
        )
        .expect("rpc.cancel notify");

    // The connection keeps serving: the cancel of a nonexistent id is dropped.
    let _: methods::TaskListResult = agent
        .call(methods::TASK_LIST, &methods::TaskListParams {})
        .await
        .expect("the connection stays alive after an rpc.cancel of an unknown id");
}

// ---------- fs.search (liveSearch T4) ----------

/// `fs.search` params with only a name glob (test helper).
pub(super) fn search_by_name(root: &str, name_glob: &str) -> FsSearchParams {
    FsSearchParams {
        name_glob: Some(name_glob.into()),
        ..FsSearchParams::new(vp(root))
    }
}

/// Drains `search.hits` + `task.progress` of a search up to its terminal.
/// After seeing the terminal, it keeps briefly draining `search.hits` already
/// queued (the hits pump and the progress pump are different tasks: the
/// order between the last batch and the terminal is not guaranteed). Returns
/// the accumulated entries and the terminal state.
pub(super) async fn drain_search(c: &mut Client, task_id: u64) -> (Vec<Entry>, TaskState) {
    let mut hits: Vec<Entry> = Vec::new();
    let mut terminal: Option<TaskState> = None;
    loop {
        // Before the terminal, wait generously; after, only drain what is
        // already queued (the walker finished, nothing new will arrive).
        let to = if terminal.is_some() {
            Duration::from_millis(400)
        } else {
            Duration::from_secs(5)
        };
        let n = match tokio::time::timeout(to, c.notification()).await {
            Ok(Some(n)) => n,
            Ok(None) => break,
            Err(_) => {
                assert!(terminal.is_some(), "timeout waiting for the search");
                break;
            }
        };
        if n.method == methods::SEARCH_HITS {
            let sh: SearchHits =
                serde_json::from_value(n.params.expect("params")).expect("SearchHits");
            if sh.task_id.get() == task_id {
                hits.extend(sh.entries);
            }
        } else if n.method == methods::TASK_PROGRESS {
            let p: TaskProgress =
                serde_json::from_value(n.params.expect("params")).expect("TaskProgress");
            if p.task_id.get() == task_id && p.state.is_terminal() {
                terminal = Some(p.state);
            }
        }
    }
    (hits, terminal.expect("the search's terminal state"))
}

/// Round-trip: A launches `fs.search`, receives `FsTaskResult`, then
/// `search.hits` with its entries and a terminal Completed `task.progress`.
#[tokio::test]
async fn fs_search_round_trip() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///sub")).await.expect("mkdir");
    write_file(&d.mem, "mem:///x.rs", b"fn main() {}").await;
    write_file(&d.mem, "mem:///y.txt", b"nope").await;
    write_file(&d.mem, "mem:///sub/z.rs", b"mod z;").await;
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(methods::FS_SEARCH, &search_by_name("mem:///", "*.rs"))
        .await
        .expect("fs.search");
    assert!(task.task_id.get() > 0);

    let (hits, state) = drain_search(&mut c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    let mut names: Vec<String> = hits
        .iter()
        .map(|e| String::from_utf8_lossy(e.path.file_name().expect("name").as_bytes()).into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["x.rs".to_string(), "z.rs".to_string()]);
}

/// Hits go ONLY to the owner: A searches, B (another connection) never
/// receives a `search.hits` (though it does see the `task.progress`, which
/// broadcasts to humans).
#[tokio::test]
async fn fs_search_hits_go_only_to_the_owner() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///a.rs", b"x").await;
    write_file(&d.mem, "mem:///b.rs", b"y").await;
    let mut a = connected_client(&d).await;
    let mut b = connected_client(&d).await;

    let task: FsTaskResult = a
        .call(methods::FS_SEARCH, &search_by_name("mem:///", "*.rs"))
        .await
        .expect("A's fs.search");
    let task_id = task.task_id.get();

    // B watches until A's task terminal and NEVER sees a search.hits.
    let mut b_saw_hits = false;
    loop {
        let notif = tokio::time::timeout(Duration::from_secs(5), b.notification())
            .await
            .expect("B's notif before the timeout")
            .expect("B's connection alive");
        if notif.method == methods::SEARCH_HITS {
            b_saw_hits = true;
        } else if notif.method == methods::TASK_PROGRESS {
            let prog: TaskProgress =
                serde_json::from_value(notif.params.expect("params")).expect("TaskProgress");
            if prog.task_id.get() == task_id && prog.state.is_terminal() {
                break;
            }
        }
    }
    assert!(!b_saw_hits, "B never receives A's search hits");

    // A did receive them.
    let (hits, state) = drain_search(&mut a, task_id).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(hits.len(), 2);
}

/// Cancellation over the wire: A searches (with latency), sends
/// `task.cancel` → terminal Cancelled; the task leaves the live ones (no
/// leak).
#[tokio::test]
async fn fs_search_cancel_over_the_wire() {
    let d = spawn_daemon(None).await;
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(20)));
    for i in 0..200 {
        write_file(&d.mem, &format!("mem:///f{i}.rs"), b"x").await;
    }
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(methods::FS_SEARCH, &search_by_name("mem:///", "*.rs"))
        .await
        .expect("fs.search");
    let _: TaskCancelResult = c
        .call(
            methods::TASK_CANCEL,
            &TaskCancelParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("task.cancel");
    let (_hits, state) = drain_search(&mut c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Cancelled);

    // It is not left as a LIVE task (only its terminal can appear in `recent`).
    let list: methods::TaskListResult = c
        .call(methods::TASK_LIST, &methods::TaskListParams::default())
        .await
        .expect("task.list");
    assert!(
        list.tasks
            .iter()
            .all(|t| t.task_id != task.task_id || t.state.is_terminal()),
        "the task is not still alive"
    );
}

/// Rows belong to whoever launched the comparison: another connection NEVER
/// sees another's `compare.rows` (same directional criterion as
/// `search.hits`).
#[tokio::test]
async fn fs_compare_rows_go_only_to_the_owner() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    write_file(&d.mem, "mem:///l/a.txt", b"x").await;
    write_file(&d.mem, "mem:///r/a.txt", b"x").await;
    let mut owner = connected_client(&d).await;
    let mut other = connected_client(&d).await;

    let task: FsTaskResult = owner
        .call(methods::FS_COMPARE, &compare_params("mem:///l", "mem:///r"))
        .await
        .expect("the owner's fs.compare");
    let task_id = task.task_id.get();

    // The other connection watches until the terminal and never sees a
    // compare.rows.
    loop {
        let notif = tokio::time::timeout(Duration::from_secs(5), other.notification())
            .await
            .expect("timeout waiting for the terminal on the other connection")
            .expect("channel alive");
        assert_ne!(
            notif.method,
            methods::COMPARE_ROWS,
            "another connection received rows that are not its own"
        );
        if notif.method == methods::TASK_PROGRESS {
            let p: TaskProgress =
                serde_json::from_value(notif.params.expect("params")).expect("TaskProgress");
            if p.task_id.get() == task_id && p.state.is_terminal() {
                break;
            }
        }
    }
    let (batches, state) = drain_compare(&mut owner, task_id).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(batches.iter().map(|b| b.rows.len()).sum::<usize>(), 1);
}

/// Cancellation over the wire (hard rule 3): `task.cancel` ends the Task as
/// `Cancelled` — the engine's ONLY `Err` IS cancellation, not a failure — and
/// the batches stop.
#[tokio::test]
async fn fs_compare_cancel_over_the_wire() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    for i in 0..200 {
        write_file(&d.mem, &format!("mem:///l/f{i}.txt"), b"x").await;
        write_file(&d.mem, &format!("mem:///r/f{i}.txt"), b"x").await;
    }
    d.mem
        .faults()
        .set_latency_per_op(Some(Duration::from_millis(20)));
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(methods::FS_COMPARE, &compare_params("mem:///l", "mem:///r"))
        .await
        .expect("fs.compare");
    let _: TaskCancelResult = c
        .call(
            methods::TASK_CANCEL,
            &TaskCancelParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("task.cancel");
    let (batches, state) = drain_compare(&mut c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Cancelled);
    assert!(
        batches.iter().map(|b| b.rows.len()).sum::<usize>() < 200,
        "rows kept arriving after the cancel"
    );

    // It is not left as a LIVE task (no leak).
    let list: methods::TaskListResult = c
        .call(methods::TASK_LIST, &methods::TaskListParams::default())
        .await
        .expect("task.list");
    assert!(
        list.tasks
            .iter()
            .all(|t| t.task_id != task.task_id || t.state.is_terminal()),
        "the task is not still alive"
    );
}

/// Two roots that resolve to the same place are `-32602` and create NO Task:
/// comparing something against itself for an hour is not a request, it is
/// the caller's typo.
#[tokio::test]
async fn fs_compare_against_itself_is_invalid_params_with_no_task() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///data")).await.expect("mkdir");
    let c = connected_client(&d).await;
    let before: methods::TaskListResult = c
        .call(methods::TASK_LIST, &methods::TaskListParams::default())
        .await
        .expect("task.list");

    let err = c
        .call::<_, FsTaskResult>(
            methods::FS_COMPARE,
            &compare_params("mem:///data", "mem:///data"),
        )
        .await
        .expect_err("rejected");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("expected Rpc INVALID_PARAMS, got {other:?}"),
    }
    let after: methods::TaskListResult = c
        .call(methods::TASK_LIST, &methods::TaskListParams::default())
        .await
        .expect("task.list");
    assert_eq!(
        after.tasks.len(),
        before.tasks.len(),
        "no Task at all can be created"
    );
}

/// And the WELL-written side reaches the engine: the left orphan gets
/// enumerated, and the right one is still just a row. It is the whole
/// cable — wire, engine, walk — not just the struct.
#[tokio::test]
async fn fs_compare_descend_orphans_reaches_the_engine() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    d.mem.mkdir(&vp("mem:///l/solo")).await.expect("mkdir solo");
    write_file(&d.mem, "mem:///l/solo/dentro.txt", b"x").await;
    d.mem.mkdir(&vp("mem:///r/otro")).await.expect("mkdir otro");
    // With a child: without it, "the other side was not descended" would
    // hold trivially.
    write_file(&d.mem, "mem:///r/otro/dentro-derecha.txt", b"y").await;
    let mut c = connected_client(&d).await;

    let mut p = compare_params("mem:///l", "mem:///r");
    p.descend_orphans = Some(methods::DescendSide::Left);
    let task: FsTaskResult = c
        .call(methods::FS_COMPARE, &p)
        .await
        .expect("fs.compare accepted");
    let (batches, terminal) = drain_compare(&mut c, task.task_id.get()).await;
    assert_eq!(terminal, TaskState::Completed);

    let names: Vec<Vec<u8>> = batches
        .iter()
        .flat_map(|b| &b.rows)
        .filter_map(|row| {
            [row.left.as_ref(), row.right.as_ref()]
                .into_iter()
                .flatten()
                .next()
                .and_then(|e| e.path.file_name())
                .map(|s| s.as_bytes().to_vec())
        })
        .collect();
    assert!(names.contains(&b"solo".to_vec()), "{names:?}");
    assert!(
        names.contains(&b"dentro.txt".to_vec()),
        "the source's orphan was not enumerated: {names:?}"
    );
    assert!(names.contains(&b"otro".to_vec()), "{names:?}");
    assert!(
        !names.contains(&b"dentro-derecha.txt".to_vec()),
        "the DESTINATION's orphan was descended: {names:?}"
    );
}

/// Steps belong to whoever launched the plan: another connection NEVER sees
/// another's `sync.steps` nor `sync.plan_done` (same directional criterion as
/// `compare.rows`). And it is more serious here: `plan_hash` IS the
/// authorization to write.
#[tokio::test]
async fn sync_plan_steps_and_hash_go_only_to_the_owner() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    write_file(&d.mem, "mem:///s/a.txt", b"x").await;
    let mut owner = connected_client(&d).await;
    let mut other = connected_client(&d).await;

    let task: FsTaskResult = owner
        .call(methods::SYNC_PLAN, &sync_params("mem:///s", "mem:///d"))
        .await
        .expect("the owner's sync.plan");
    let task_id = task.task_id.get();

    loop {
        let notif = tokio::time::timeout(Duration::from_secs(5), other.notification())
            .await
            .expect("timeout waiting for the terminal on the other connection")
            .expect("channel alive");
        assert_ne!(
            notif.method,
            methods::SYNC_STEPS,
            "steps that are not its own"
        );
        assert_ne!(
            notif.method,
            methods::SYNC_PLAN_DONE,
            "another's plan_hash is another's write authorization"
        );
        if notif.method == methods::TASK_PROGRESS {
            let p: TaskProgress =
                serde_json::from_value(notif.params.expect("params")).expect("TaskProgress");
            if p.task_id.get() == task_id && p.state.is_terminal() {
                break;
            }
        }
    }
    let (batches, done, state) = drain_sync(&mut owner, task_id).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(batches.iter().map(|b| b.steps.len()).sum::<usize>(), 1);
    assert!(done.is_some());
}

/// Third of the spool's four deaths (ADR 0049): closing the connection takes
/// its plans with it. An ownerless plan cannot be applied by anyone, and what
/// would be left on disk is a relative listing of two trees.
#[tokio::test]
async fn sync_plan_closing_the_connection_takes_its_plans_with_it() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    write_file(&d.mem, "mem:///s/a.txt", b"x").await;
    // The socket lives in the same tempdir as the daemon's state.
    let spool_dir = d
        .socket
        .parent()
        .expect("the socket hangs off the tempdir")
        .join(norte_core::sync::SPOOL_DIR_NAME);
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(methods::SYNC_PLAN, &sync_params("mem:///s", "mem:///d"))
        .await
        .expect("sync.plan");
    let (_, done, state) = drain_sync(&mut c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    assert!(done.is_some());
    assert_eq!(
        std::fs::read_dir(&spool_dir).expect("spool dir").count(),
        1,
        "the approved plan is retained while its connection is alive"
    );

    drop(c);
    // Tearing down the connection is asynchronous. It waits FOR THE CONDITION
    // with a budget, never a fixed span: an eyeballed `sleep` is what turns a
    // test intermittent under load.
    let until = tokio::time::Instant::now() + Duration::from_secs(10);
    let remaining = loop {
        let n = std::fs::read_dir(&spool_dir).expect("spool dir").count();
        if n == 0 || tokio::time::Instant::now() >= until {
            break n;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert_eq!(remaining, 0, "an ownerless plan is applied by nobody");
}

/// Happy path over the socket: build → embed → `search_semantic` returns the
/// seeded file as the first hit, with a score that is a FINITE JSON number
/// (the engine's anti-NaN belt is contractual: a `NaN` would serialize as
/// `null` and break the response on the client).
#[tokio::test]
async fn semantic_over_the_socket_returns_hits() {
    let d = spawn_daemon_embed(None).await;
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    write_file(&d.mem, "mem:///r/a.txt", b"contenido alfa").await;
    let mut c = connected_client(&d).await;

    let t: FsTaskResult = c
        .call(
            methods::INDEX_BUILD,
            &methods::IndexBuildParams {
                root: vp("mem:///r"),
            },
        )
        .await
        .expect("index.build");
    let seen = drain_task(&mut c, t.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);

    let t: FsTaskResult = c
        .call(
            methods::INDEX_EMBED,
            &methods::IndexEmbedParams {
                root: vp("mem:///r"),
            },
        )
        .await
        .expect("index.embed");
    let seen = drain_task(&mut c, t.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);

    let r: methods::IndexSearchSemanticResult = c
        .call(
            methods::INDEX_SEARCH_SEMANTIC,
            &methods::IndexSearchSemanticParams {
                root: Some(vp("mem:///r")),
                query: "contenido alfa".into(),
                k: 5,
            },
        )
        .await
        .expect("index.search_semantic");
    let top = r.hits.first().expect("at least one hit");
    assert_eq!(top.path, vp("mem:///r/a.txt"));
    assert!(
        top.score.is_finite(),
        "finite score by the wire's contract, was {}",
        top.score
    );
}

/// And with a HOSTILE name, byte for byte over the socket (#122).
///
/// The test above rounds off with `a.txt`, so the path that comes back fits
/// in ASCII and says nothing about the NDJSON path. Here the file carries
/// non-UTF-8 bytes (`%FF%FE` on the wire), which is exactly what an extra
/// `to_string_lossy` would turn into `\u{FFFD}` — giving a hit that points at
/// a file that does not exist, and that a frontend would `cd` into finding
/// nothing.
#[tokio::test]
async fn semantic_over_the_socket_survives_a_hostile_name() {
    let d = spawn_daemon_embed(None).await;
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    let hostile = "mem:///r/%FF%FE.txt";
    write_file(&d.mem, hostile, b"contenido alfa").await;
    let mut c = connected_client(&d).await;

    let t: FsTaskResult = c
        .call(
            methods::INDEX_BUILD,
            &methods::IndexBuildParams {
                root: vp("mem:///r"),
            },
        )
        .await
        .expect("index.build");
    let seen = drain_task(&mut c, t.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);

    let t: FsTaskResult = c
        .call(
            methods::INDEX_EMBED,
            &methods::IndexEmbedParams {
                root: vp("mem:///r"),
            },
        )
        .await
        .expect("index.embed");
    let seen = drain_task(&mut c, t.task_id.get()).await;
    assert_eq!(seen.last().expect("terminal").state, TaskState::Completed);

    let r: methods::IndexSearchSemanticResult = c
        .call(
            methods::INDEX_SEARCH_SEMANTIC,
            &methods::IndexSearchSemanticParams {
                root: Some(vp("mem:///r")),
                query: "contenido alfa".into(),
                k: 5,
            },
        )
        .await
        .expect("index.search_semantic");
    let top = r.hits.first().expect("at least one hit");
    assert_eq!(
        top.path,
        vp(hostile),
        "the path came back from the socket with different bytes"
    );
    // And the bytes are the real ones, not a replacement: `\u{FFFD}` in UTF-8
    // is `efbfbd`, and comparing the reconstructed path would not tell it
    // apart if the parser had accepted the escape of something else.
    assert_eq!(
        top.path.file_name().expect("name").as_bytes(),
        b"\xff\xfe.txt"
    );
}

/// The case a per-pair `fs.move` loop could NEVER do: an `a→b, b→a`
/// permutation as ONE Task over the socket.
#[tokio::test]
async fn rename_batch_runs_a_permutation_over_the_socket() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///a", b"soy-a").await;
    write_file(&d.mem, "mem:///b", b"soy-b").await;
    let c = connected_client(&d).await;
    let pairs = vec![pair(b"a", b"b"), pair(b"b", b"a")];

    let plan: methods::FsRenameBatchPlanResult = c
        .call(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///"),
                pairs: pairs.clone(),
            },
        )
        .await
        .expect("plan");
    assert!(plan.executable, "{:?}", plan.collisions);
    assert_eq!(plan.steps.len(), 3, "two renames and one temporary");

    let task: FsTaskResult = c
        .call(
            methods::FS_RENAME_BATCH,
            &methods::FsRenameBatchParams {
                dir: vp("mem:///"),
                pairs,
                plan_hash: plan.plan_hash.clone(),
            },
        )
        .await
        .expect("fs.rename_batch");
    assert_eq!(wait_terminal(&c, task.task_id).await, TaskState::Completed);
    assert_eq!(read_all(&d.mem, "mem:///a").await, b"soy-b");
    assert_eq!(read_all(&d.mem, "mem:///b").await, b"soy-a");

    // The batch's report over the wire: the run was clean.
    let report: methods::FsRenameBatchReportResult = c
        .call(
            methods::FS_RENAME_BATCH_REPORT,
            &methods::FsRenameBatchReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("fs.rename_batch_report");
    assert_eq!(report.applied, 3);
    assert_eq!(report.rolled_back, 0);
    assert!(report.stuck.is_none());
    assert!(report.uncertain.is_none());
    assert_eq!(report.compensations_lost, 0);
}

/// A batch that fails midway unwinds entirely, and the REPORT counts it over
/// the wire: the Task's `Failed` only tells the cause.
#[tokio::test]
async fn a_failed_rename_batch_counts_its_rollback_over_the_socket() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///a", b"1").await;
    write_file(&d.mem, "mem:///b", b"2").await;
    let c = connected_client(&d).await;
    let pairs = vec![pair(b"a", b"x"), pair(b"b", b"y")];
    let plan: methods::FsRenameBatchPlanResult = c
        .call(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///"),
                pairs: pairs.clone(),
            },
        )
        .await
        .expect("plan");
    // The SECOND step dies; the first already applied and has to be undone.
    d.mem.faults().fail_rename_at(&vp("mem:///b"));

    let task: FsTaskResult = c
        .call(
            methods::FS_RENAME_BATCH,
            &methods::FsRenameBatchParams {
                dir: vp("mem:///"),
                pairs,
                plan_hash: plan.plan_hash,
            },
        )
        .await
        .expect("fs.rename_batch");
    assert!(
        matches!(
            wait_terminal(&c, task.task_id).await,
            TaskState::Failed { .. }
        ),
        "the whole batch fails"
    );
    assert!(d.mem.stat(&vp("mem:///a")).await.is_ok(), "a came back");
    assert!(matches!(
        d.mem.stat(&vp("mem:///x")).await,
        Err(Error::NotFound)
    ));

    let report: methods::FsRenameBatchReportResult = c
        .call(
            methods::FS_RENAME_BATCH_REPORT,
            &methods::FsRenameBatchReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("fs.rename_batch_report");
    assert_eq!(report.applied, 1);
    assert_eq!(report.rolled_back, 1);
    assert_eq!(report.failed_pair, Some(1), "the `b → y` row");
    assert!(report.stuck.is_none(), "the rollback DID manage to finish");
}

/// A `task_id` that was never a batch is `INVALID_PARAMS`, not a blank report
/// that could be read as "everything went fine".
#[tokio::test]
async fn rename_batch_report_of_an_unknown_task_is_invalid_params() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::FsRenameBatchReportResult>(
            methods::FS_RENAME_BATCH_REPORT,
            &methods::FsRenameBatchReportParams {
                task_id: norte_proto::TaskId::new(4242),
            },
        )
        .await
        .expect_err("unknown id");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::NotFound)),
            "NotFound from the taxonomy — the SAME one the embedded arm gives, \
             was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// The report of ANOTHER actor's batch answers the same as an id that never
/// existed: it carries paths from another actor's directory, and
/// distinguishing "not yours" from "does not exist" would already confirm it
/// existed (same criterion as `task.cancel`). The human, by contrast, sees
/// the agent's batch report — they are the one who has to clean up if it got
/// stuck.
#[tokio::test]
async fn an_agent_does_not_read_another_actors_batch_report() {
    let d = spawn_daemon_policy().await;
    d.mem.mkdir(&vp("mem:///proj")).await.expect("mkdir");
    write_file(&d.mem, "mem:///proj/a", b"1").await;
    let human = connected_client(&d).await;
    let agent = connected_agent(&d, "s1").await;

    // The HUMAN's batch.
    let pairs = vec![pair(b"a", b"b")];
    let plan: methods::FsRenameBatchPlanResult = human
        .call(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///proj"),
                pairs: pairs.clone(),
            },
        )
        .await
        .expect("plan");
    let task: FsTaskResult = human
        .call(
            methods::FS_RENAME_BATCH,
            &methods::FsRenameBatchParams {
                dir: vp("mem:///proj"),
                pairs,
                plan_hash: plan.plan_hash,
            },
        )
        .await
        .expect("the human's batch");
    assert_eq!(
        wait_terminal(&human, task.task_id).await,
        TaskState::Completed
    );

    // The agent asks for THAT report: an answer indistinguishable from a
    // made-up id.
    let others = agent
        .call::<_, methods::FsRenameBatchReportResult>(
            methods::FS_RENAME_BATCH_REPORT,
            &methods::FsRenameBatchReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect_err("another's report");
    let made_up = agent
        .call::<_, methods::FsRenameBatchReportResult>(
            methods::FS_RENAME_BATCH_REPORT,
            &methods::FsRenameBatchReportParams {
                task_id: norte_proto::TaskId::new(9999),
            },
        )
        .await
        .expect_err("made-up id");
    match (others, made_up) {
        (ClientError::Rpc(a), ClientError::Rpc(b)) => {
            assert!(matches!(a.data, Some(Error::NotFound)), "{:?}", a.data);
            assert_eq!(
                (a.code, a.message),
                (b.code, b.message),
                "the two answers have to be THE SAME",
            );
        }
        other => panic!("expected two Rpc, got {other:?}"),
    }

    // And the human does read it.
    let report: methods::FsRenameBatchReportResult = human
        .call(
            methods::FS_RENAME_BATCH_REPORT,
            &methods::FsRenameBatchReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("the owner reads its report");
    assert_eq!(report.applied, 1);
}

// -------------------------------------------------- sync.apply / sync.report

/// The `sync.plan` of a three-file tree, already closed, from the connection
/// that requested it. Returns the close — `plan_hash` is there and nowhere
/// else — so it can be applied.
pub(super) async fn plan_over(d: &TestDaemon, c: &mut Client) -> methods::SyncPlanDone {
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    write_file(&d.mem, "mem:///s/a.txt", b"aaa").await;
    write_file(&d.mem, "mem:///s/b.txt", b"bbbb").await;
    // And one that ALREADY exists at the destination with a different SIZE: an
    // overwrite via the size rung (`Certain`), so the report counts more than
    // just copies. With both the same size the cascade would fall to the
    // date, and the `MemProvider`'s logical clock advances one at a time: two
    // files written back to back fall within the tolerance and come out
    // `Same`, i.e. no step.
    write_file(&d.mem, "mem:///s/c.txt", b"nuevo, y mas largo").await;
    write_file(&d.mem, "mem:///d/c.txt", b"viejo").await;

    let task: FsTaskResult = c
        .call(methods::SYNC_PLAN, &sync_params("mem:///s", "mem:///d"))
        .await
        .expect("sync.plan");
    let (_batches, done, state) = drain_sync(c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    done.expect("the plan closed with its hash")
}

/// Applying the approved plan RUNS it, and the report counts what happened:
/// as many steps done as copies and overwrites the plan carried, and a
/// journal batch to look them up under — without it there is no undo to ask
/// for.
#[tokio::test]
async fn sync_apply_runs_the_plan_that_was_approved() {
    let d = spawn_daemon_journal().await;
    let mut c = connected_client(&d).await;
    let done = plan_over(&d, &mut c).await;
    assert!(done.executable);

    let task: FsTaskResult = c
        .call(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash.clone(),
            },
        )
        .await
        .expect("sync.apply accepted");
    assert_eq!(
        wait_terminal(&c, task.task_id).await,
        TaskState::Completed,
        "the plan applied entirely"
    );

    let report: methods::SyncReportResult = c
        .call(
            methods::SYNC_REPORT,
            &methods::SyncReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("sync.report");
    assert_eq!(report.done, done.counts.copy + done.counts.overwrite);
    assert_eq!(report.failed, 0, "{:?}", report.failures);
    assert!(report.batch_id.is_some(), "undo needs it");
    // And the tree for real: the destination has the source's bytes.
    let mut stream = d
        .mem
        .read(&vp("mem:///d/c.txt"), None)
        .await
        .expect("read the destination");
    let mut bytes = Vec::new();
    while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
        bytes.extend_from_slice(&chunk.expect("chunk"));
    }
    assert_eq!(bytes, b"nuevo, y mas largo", "the overwrite happened");
    assert_eq!(done.counts.overwrite, 1, "the plan carried one overwrite");
}

/// The report is seen by whoever could see the Task: its owner or any HUMAN
/// connection. An agent asking about another's report receives the SAME
/// answer as for a made-up id — the report carries relative paths of two
/// trees it has no business seeing, and distinguishing "not yours" from
/// "does not exist" would already leak that it existed.
#[tokio::test]
async fn another_actors_sync_report_answers_the_same_as_a_made_up_id() {
    let d = spawn_daemon_journal().await;
    let mut owner = connected_client(&d).await;
    let done = plan_over(&d, &mut owner).await;
    let task: FsTaskResult = owner
        .call(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash,
            },
        )
        .await
        .expect("sync.apply");
    assert_eq!(
        wait_terminal(&owner, task.task_id).await,
        TaskState::Completed
    );

    let snoop = connected_agent(&d, "s-fisgona").await;
    let others = snoop
        .call::<_, methods::SyncReportResult>(
            methods::SYNC_REPORT,
            &methods::SyncReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect_err("not theirs");
    let made_up = snoop
        .call::<_, methods::SyncReportResult>(
            methods::SYNC_REPORT,
            &methods::SyncReportParams {
                task_id: norte_proto::TaskId::new(99_999),
            },
        )
        .await
        .expect_err("never existed");
    for err in [others, made_up] {
        match err {
            ClientError::Rpc(rpc) => assert!(
                matches!(rpc.data, Some(Error::NotFound)),
                "NotFound, was {:?}",
                rpc.data
            ),
            other => panic!("expected Rpc, got {other:?}"),
        }
    }
    // Another HUMAN connection does see it: `may_observe`'s symmetry.
    let other_human = connected_client(&d).await;
    let _: methods::SyncReportResult = other_human
        .call(
            methods::SYNC_REPORT,
            &methods::SyncReportParams {
                task_id: task.task_id,
            },
        )
        .await
        .expect("a human sees the reports of the daemon it governs");
}

/// Every new span, with the chain of its ancestors' names (inside out) and
/// the fields it was born with.
/// `(name, ancestors inside out, fields)`.
type SpanSeen = (String, Vec<String>, String);

#[derive(Clone, Default)]
struct Spans(Arc<std::sync::Mutex<Vec<SpanSeen>>>);

impl<S> tracing_subscriber::Layer<S> for Spans
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct Fields(String);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                use std::fmt::Write as _;
                let _ = write!(self.0, "{}={v:?};", f.name());
            }
        }
        let mut fields = Fields(String::new());
        attrs.record(&mut fields);
        let Some(span) = ctx.span(id) else { return };
        let ancestors = span.scope().skip(1).map(|s| s.name().to_owned()).collect();
        self.0
            .lock()
            .expect("spans")
            .push((span.name().to_owned(), ancestors, fields.0));
    }
}

/// **A task the daemon requests hangs off the request that asked for it**
/// (ADR 0127), and the request is ONE `rpc` span — not two: `dispatch` already
/// carried its own `#[instrument]`, and with both stacked the chain was
/// `dispatch → rpc → task`, with the method repeated and `spans[0]` wrong.
#[tokio::test]
async fn a_requests_task_hangs_off_its_rpc() {
    use tracing_subscriber::layer::SubscriberExt as _;

    let spans = Spans::default();
    let _guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(spans.clone()));

    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///src.bin", &[0xAB; 10]).await;
    let mut c = connected_client(&d).await;
    let task: FsTaskResult = c
        .call(
            methods::FS_COPY,
            &FsCopyParams {
                from: vp("mem:///src.bin"),
                to: vp("mem:///dst.bin"),
                on_collision: norte_proto::CollisionPolicy::default(),
                symlinks: norte_proto::SymlinkPolicy::default(),
                resume: norte_proto::ResumePolicy::default(),
                verify: norte_proto::VerifyPolicy::default(),
                dest_anchor: None,
                queued: false,
            },
        )
        .await
        .expect("fs.copy");
    let _ = drain_task(&mut c, task.task_id.get()).await;

    let seen = spans.0.lock().expect("spans").clone();
    assert!(
        !seen.iter().any(|(n, _, _)| n == "dispatch"),
        "dispatch opens ONE span, `rpc`: {seen:?}"
    );
    let copy = seen
        .iter()
        .find(|(n, _, c)| n == "rpc" && c.contains("method=fs.copy"))
        .expect("fs.copy's rpc");
    assert!(copy.1.is_empty(), "rpc is the root: {copy:?}");
    assert!(copy.2.contains("conn_id="), "{copy:?}");
    assert!(copy.2.contains("req_id="), "{copy:?}");
    let task_span = seen
        .iter()
        .find(|(n, _, c)| n == "task" && c.contains(&format!("task_id={}", task.task_id)))
        .expect("the task's span");
    assert!(
        task_span.1.iter().any(|a| a == "rpc"),
        "the task hangs off its request: {task_span:?}"
    );
}
