use super::*;

// ---------- fs.compare (C6 of the directory-comparison plan) ----------

/// `fs.compare` params with the default criteria (no hash).
pub(super) fn compare_params(left: &str, right: &str) -> methods::FsCompareParams {
    methods::FsCompareParams {
        left: vp(left),
        right: vp(right),
        criteria: methods::CompareCriteria::default(),
        max_depth: None,
        mtime_tolerance_ms: 2000,
        follow_symlinks: false,
        descend_orphans: None,
    }
}

/// Drains `compare.rows` + `task.progress` of a comparison up to its
/// terminal. Same criterion as [`drain_search`]: after the terminal,
/// whatever was already queued is still briefly drained (the two pumps are
/// different tasks). Returns the BATCHES and the terminal state.
pub(super) async fn drain_compare(
    c: &mut Client,
    task_id: u64,
) -> (Vec<methods::CompareRowsBatch>, TaskState) {
    let mut batches: Vec<methods::CompareRowsBatch> = Vec::new();
    let mut terminal: Option<TaskState> = None;
    loop {
        let to = if terminal.is_some() {
            Duration::from_millis(400)
        } else {
            Duration::from_secs(5)
        };
        let n = match tokio::time::timeout(to, c.notification()).await {
            Ok(Some(n)) => n,
            Ok(None) => break,
            Err(_) => {
                assert!(terminal.is_some(), "timeout waiting for the comparison");
                break;
            }
        };
        if n.method == methods::COMPARE_ROWS {
            let b: methods::CompareRowsBatch =
                serde_json::from_value(n.params.expect("params")).expect("CompareRowsBatch");
            if b.task_id.get() == task_id {
                batches.push(b);
            }
        } else if n.method == methods::TASK_PROGRESS {
            let p: TaskProgress =
                serde_json::from_value(n.params.expect("params")).expect("TaskProgress");
            if p.task_id.get() == task_id && p.state.is_terminal() {
                terminal = Some(p.state);
            }
        }
    }
    (batches, terminal.expect("the comparison's terminal state"))
}

/// Round-trip over the socket: rows arrive in batches CAPPED by
/// `COMPARE_ROWS_MAX_BATCH`, coalesced, and the Task ends up `Completed`.
#[tokio::test]
async fn fs_compare_round_trip_in_capped_batches() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    for i in 0..600 {
        write_file(&d.mem, &format!("mem:///l/f{i}.txt"), b"x").await;
        write_file(&d.mem, &format!("mem:///r/f{i}.txt"), b"x").await;
    }
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(methods::FS_COMPARE, &compare_params("mem:///l", "mem:///r"))
        .await
        .expect("fs.compare");
    let (batches, state) = drain_compare(&mut c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    assert!(
        batches
            .iter()
            .all(|b| b.rows.len() <= methods::COMPARE_ROWS_MAX_BATCH),
        "batch above the cap"
    );
    let rows: usize = batches.iter().map(|b| b.rows.len()).sum();
    assert_eq!(rows, 600, "one row per pair");
    assert!(batches.len() < 600, "one frame per row is not coalescing");
    assert!(
        batches
            .iter()
            .flat_map(|b| &b.rows)
            .all(|r| r.sides_are_consistent() && r.reason_is_consistent()),
        "the daemon cannot emit inconsistent rows"
    );
}

/// `follow_symlinks: true` is `-32602`: the engine accepts the field and
/// IGNORES it, and silently serving a different walk than the one requested
/// is worse than not offering it.
#[tokio::test]
async fn fs_compare_follow_symlinks_is_invalid_params() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    let c = connected_client(&d).await;

    let mut p = compare_params("mem:///l", "mem:///r");
    p.follow_symlinks = true;
    let err = c
        .call::<_, FsTaskResult>(methods::FS_COMPARE, &p)
        .await
        .expect_err("rejected");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("expected Rpc INVALID_PARAMS, got {other:?}"),
    }
}

/// A MISSPELLED side is `-32602` over the socket, not "no side".
///
/// What rejects it is the TYPE (`DescendSide` has no `serde(other)`), not a
/// handler `if`: `parse_params` never gets to build the request. The test
/// still lives here because what has to be guaranteed is the response the
/// client sees, and because if someone softened the type to `Side` — which
/// does degrade — this is the test that would go red. `"unknown"` is in the
/// list on purpose: it is the value `Side` would accept and that means "no
/// side".
#[tokio::test]
async fn fs_compare_a_misspelled_side_is_invalid_params() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///l")).await.expect("mkdir l");
    d.mem.mkdir(&vp("mem:///r")).await.expect("mkdir r");
    let c = connected_client(&d).await;

    for bad in ["lft", "unknown", "both"] {
        let err = c
            .call::<_, FsTaskResult>(
                methods::FS_COMPARE,
                &serde_json::json!({
                    "left": "mem:///l",
                    "right": "mem:///r",
                    "descend_orphans": bad,
                }),
            )
            .await
            .expect_err("rejected");
        match err {
            ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS, "{bad}"),
            other => panic!("expected Rpc INVALID_PARAMS for {bad}, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------- sync.plan

pub(super) fn sync_params(source: &str, dest: &str) -> methods::SyncPlanParams {
    methods::SyncPlanParams {
        source: vp(source),
        dest: vp(dest),
        mode: methods::SyncMode::Update,
        compare: methods::SyncCompareOptions::default(),
        on_unknown: methods::OnUnknown::Copy,
        include: None,
    }
}

/// Drains `sync.steps` + `sync.plan_done` + `task.progress` of a plan up to
/// its terminal. Returns the batches IN ORDER, the close (if there was one)
/// and the state.
///
/// The order matters and that is why it is not discarded: `sync.plan_done`
/// CLOSES the plan, and a batch after it would be a client approving the hash
/// of a plan that was still arriving.
pub(super) async fn drain_sync(
    c: &mut Client,
    task_id: u64,
) -> (
    Vec<methods::SyncStepsBatch>,
    Option<methods::SyncPlanDone>,
    TaskState,
) {
    let mut batches = Vec::new();
    let mut done: Option<methods::SyncPlanDone> = None;
    let mut terminal = None;
    let mut progress = None;
    loop {
        let next = tokio::time::timeout(Duration::from_secs(10), c.notification()).await;
        let n = match next {
            Ok(Some(n)) => n,
            Ok(None) => break,
            Err(_) => {
                assert!(terminal.is_some(), "timeout waiting for the plan");
                break;
            }
        };
        if n.method == methods::SYNC_STEPS {
            let b: methods::SyncStepsBatch =
                serde_json::from_value(n.params.expect("params")).expect("SyncStepsBatch");
            if b.task_id.get() == task_id {
                assert!(
                    done.is_none(),
                    "a sync.steps AFTER sync.plan_done: the close has to be last"
                );
                batches.push(b);
            }
        } else if n.method == methods::SYNC_PLAN_DONE {
            let d: methods::SyncPlanDone =
                serde_json::from_value(n.params.expect("params")).expect("SyncPlanDone");
            if d.task_id.get() == task_id {
                assert!(done.is_none(), "two closes for one plan");
                done = Some(d);
            }
        } else if n.method == methods::TASK_PROGRESS {
            let p: TaskProgress =
                serde_json::from_value(n.params.expect("params")).expect("TaskProgress");
            if p.task_id.get() == task_id && p.state.is_terminal() {
                // The progress contract of a `SyncPlan` Task, as its rustdoc
                // publishes it: counts STEPS and not bytes, and
                // `entries_done` is the ONLY signal a client has to detect a
                // lost `sync.steps`. Without this, the two sentences are
                // just prose.
                assert_eq!(p.kind, norte_proto::TaskKind::SyncPlan);
                assert_eq!(p.bytes_done, 0, "planning does not write a byte");
                assert!(p.current.is_none(), "no path in the broadcast");
                progress = Some(p.entries_done);
                terminal = Some(p.state);
                // The close can come AFTER the terminal: draining continues
                // until the short timeout above says nothing is left.
            }
        }
    }
    if let (Some(entries), Some(TaskState::Completed)) = (progress, terminal.as_ref()) {
        let seen: u64 = batches
            .iter()
            .map(|b| u64::try_from(b.steps.len()).expect("fits"))
            .sum();
        assert_eq!(
            entries, seen,
            "entries_done has to match the delivered steps"
        );
    }
    (batches, done, terminal.expect("the plan's terminal state"))
}

/// Round-trip over the socket: the steps arrive in capped batches and
/// `sync.plan_done` CLOSES them — never the other way around.
#[tokio::test]
async fn sync_plan_round_trip_steps_and_then_the_close() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    for i in 0..600 {
        write_file(&d.mem, &format!("mem:///s/f{i}.txt"), b"x").await;
    }
    let mut c = connected_client(&d).await;

    let task: FsTaskResult = c
        .call(methods::SYNC_PLAN, &sync_params("mem:///s", "mem:///d"))
        .await
        .expect("sync.plan");
    let (batches, done, state) = drain_sync(&mut c, task.task_id.get()).await;
    assert_eq!(state, TaskState::Completed);
    assert!(
        batches
            .iter()
            .all(|b| b.steps.len() <= methods::SYNC_STEPS_MAX_BATCH),
        "batch above the cap"
    );
    let steps: usize = batches.iter().map(|b| b.steps.len()).sum();
    assert_eq!(steps, 600);
    assert!(batches.len() < 600, "one frame per step is not coalescing");
    let done = done.expect("the plan closed");
    assert_eq!(done.counts.copy, 600);
    assert!(done.executable);
    assert_eq!(done.plan_hash.as_str().len(), methods::PLAN_HASH_LEN);
}

/// The two `compare` fields that are not the caller's, and the `include`
/// cap: `-32602` WITHOUT creating a Task.
#[tokio::test]
async fn sync_plan_params_that_are_not_the_callers_are_invalid_params() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///s")).await.expect("mkdir s");
    d.mem.mkdir(&vp("mem:///d")).await.expect("mkdir d");
    let c = connected_client(&d).await;

    let mut p = sync_params("mem:///s", "mem:///d");
    p.compare.follow_symlinks = true;
    assert_invalid_params(c.call::<_, FsTaskResult>(methods::SYNC_PLAN, &p).await);

    let mut p = sync_params("mem:///s", "mem:///d");
    p.compare.descend_orphans = Some(methods::DescendSide::Right);
    assert_invalid_params(c.call::<_, FsTaskResult>(methods::SYNC_PLAN, &p).await);

    let mut p = sync_params("mem:///s", "mem:///d");
    p.include = Some(vec![
        methods::RelPath::parse_wire("x").expect("rel");
        methods::SYNC_MAX_INCLUDE + 1
    ]);
    assert_invalid_params(c.call::<_, FsTaskResult>(methods::SYNC_PLAN, &p).await);
}

/// Overlapping roots: a wire category (`OverlappingRoots`), not `-32602`, and
/// with the relation inside — a frontend paints the three differently.
#[tokio::test]
async fn sync_plan_overlapping_roots_travel_with_their_relation() {
    let d = spawn_daemon(None).await;
    d.mem.mkdir(&vp("mem:///a")).await.expect("mkdir a");
    d.mem.mkdir(&vp("mem:///a/sub")).await.expect("mkdir sub");
    let c = connected_client(&d).await;

    let err = c
        .call::<_, FsTaskResult>(methods::SYNC_PLAN, &sync_params("mem:///a", "mem:///a/sub"))
        .await
        .expect_err("overlapping");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(
                rpc.data,
                Some(Error::OverlappingRoots {
                    relation: norte_proto::RootOverlap::DestInsideSource
                })
            ),
            "was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// A hash this daemon never issued is `PlanStale`: no live plan exists with
/// that name, and that is the only thing the answer says.
#[tokio::test]
async fn sync_apply_of_a_hash_nobody_issued_is_plan_stale() {
    let d = spawn_daemon_journal().await;
    let c = connected_client(&d).await;

    let made_up = methods::PlanHash::parse(&"0".repeat(methods::PLAN_HASH_LEN)).expect("valid hex");
    let err = c
        .call::<_, FsTaskResult>(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams { plan_hash: made_up },
        )
        .await
        .expect_err("nobody issued that plan");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PlanStale)),
            "PlanStale, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
}

/// A MALFORMED hash dies in the deserializer (`-32602`) and does not disguise
/// itself as `PlanStale`: "this is not a hash" and "the world moved" are
/// different facts, and answering the second to whoever sent the first lies
/// to it about the state of the world.
#[tokio::test]
async fn sync_apply_with_a_malformed_hash_is_invalid_params_and_not_plan_stale() {
    let d = spawn_daemon_journal().await;
    let c = connected_client(&d).await;

    // Not hexadecimal, not the right length, not lowercase: the three ways
    // of not being a `PlanHash`.
    for garbage in [
        serde_json::json!({"plan_hash": "nope"}),
        serde_json::json!({"plan_hash": "0".repeat(methods::PLAN_HASH_LEN - 1)}),
        serde_json::json!({"plan_hash": "A".repeat(methods::PLAN_HASH_LEN)}),
        serde_json::json!({}),
    ] {
        assert_invalid_params(
            c.call::<_, FsTaskResult>(methods::SYNC_APPLY, &garbage)
                .await,
        );
    }
}

/// ANOTHER connection's plan is `PlanStale`, not its own category: the plan
/// is tied to the connection that produced it, and answering anything other
/// than "no live plan with that hash" would build an existence oracle over
/// other connections' plans — which are write authorizations.
#[tokio::test]
async fn sync_apply_of_another_connections_plan_is_plan_stale() {
    let d = spawn_daemon_journal().await;
    let mut owner = connected_client(&d).await;
    let done = plan_over(&d, &mut owner).await;
    let other = connected_client(&d).await;

    let err = other
        .call::<_, FsTaskResult>(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash.clone(),
            },
        )
        .await
        .expect_err("the plan is not theirs");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PlanStale)),
            "PlanStale, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
    // And the owner CAN: the rejection was about the connection, not the
    // plan.
    let _: FsTaskResult = owner
        .call(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash,
            },
        )
        .await
        .expect("its own plan does work");
}

/// A plan is approved ONCE: once the application finishes — in any state —
/// the retained plan is gone, so the same hash no longer runs anything.
/// Without this, a leaked hash would be a reusable write authorization.
#[tokio::test]
async fn the_spool_is_spent_as_soon_as_the_application_finishes() {
    let d = spawn_daemon_journal().await;
    let mut c = connected_client(&d).await;
    let done = plan_over(&d, &mut c).await;

    let task: FsTaskResult = c
        .call(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash.clone(),
            },
        )
        .await
        .expect("sync.apply accepted");
    assert_eq!(wait_terminal(&c, task.task_id).await, TaskState::Completed);

    let err = c
        .call::<_, FsTaskResult>(
            methods::SYNC_APPLY,
            &methods::SyncApplyParams {
                plan_hash: done.plan_hash,
            },
        )
        .await
        .expect_err("a plan is approved once");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PlanStale)),
            "PlanStale, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
}
