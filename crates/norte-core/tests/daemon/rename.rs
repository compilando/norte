use super::*;

// ---------- fs.rename_batch{,_plan,_report} (0.36.0, ADR 0042) ----------

/// A basename from its BYTES: what a `String` could not have carried.
pub(super) fn sg(bytes: &[u8]) -> norte_proto::Segment {
    norte_proto::Segment::new(bytes.to_vec()).expect("test segment")
}

/// A `MemProvider` file's full content (to check WHICH file ended up under
/// each name after a permutation).
pub(super) async fn read_all(mem: &MemProvider, wire: &str) -> Vec<u8> {
    use futures::StreamExt;
    let mut stream = mem.read(&vp(wire), None).await.expect("read opens");
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk"));
    }
    out
}

pub(super) fn pair(from: &[u8], to: &[u8]) -> methods::RenamePair {
    methods::RenamePair {
        from: sg(from),
        to: sg(to),
    }
}

/// The plan crosses the socket with a NON-UTF8 name intact, and mutates
/// nothing.
///
/// The name travels percent-encoded (`caf%FF.txt`) and comes back as the
/// same bytes: it is the case that motivates `RenamePair` carrying a
/// `Segment` and not a `String` (hard rule 1).
#[tokio::test]
async fn rename_batch_plan_answers_over_the_socket() {
    let d = spawn_daemon(None).await;
    let hostile = b"caf\xff.txt";
    write_file(&d.mem, "mem:///caf%FF.txt", b"x").await;
    write_file(&d.mem, "mem:///b.txt", b"y").await;
    let c = connected_client(&d).await;

    let plan: methods::FsRenameBatchPlanResult = c
        .call(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///"),
                pairs: vec![pair(hostile, b"cafe.txt")],
            },
        )
        .await
        .expect("fs.rename_batch_plan");

    assert!(plan.executable, "{:?}", plan.collisions);
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(
        plan.steps[0].from.as_bytes(),
        hostile,
        "the hostile bytes survive the round trip",
    );
    assert_eq!(plan.steps[0].to.as_bytes(), b"cafe.txt");
    assert_eq!(plan.plan_hash.to_string().len(), 64);
    // Planning does NOT mutate: the file keeps its name.
    assert!(d.mem.stat(&vp("mem:///caf%FF.txt")).await.is_ok());
    assert!(matches!(
        d.mem.stat(&vp("mem:///cafe.txt")).await,
        Err(Error::NotFound)
    ));
}

/// DRIFT is refused with the actionable category (`plan_stale`), not a
/// generic internal error: the human knows it has to plan again.
#[tokio::test]
async fn rename_batch_with_a_stale_hash_is_plan_stale() {
    let d = spawn_daemon(None).await;
    write_file(&d.mem, "mem:///a", b"1").await;
    let c = connected_client(&d).await;
    let pairs = vec![pair(b"a", b"z")];
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
    assert!(plan.executable);

    // The destination appears BEHIND the daemon's back: the re-plan sees it
    // occupied and concludes something else.
    write_file(&d.mem, "mem:///z", b"intruso").await;

    let err = c
        .call::<_, FsTaskResult>(
            methods::FS_RENAME_BATCH,
            &methods::FsRenameBatchParams {
                dir: vp("mem:///"),
                pairs,
                plan_hash: plan.plan_hash,
            },
        )
        .await
        .expect_err("the approved plan is no longer valid");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::PlanStale)),
            "PlanStale, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }
    // And it touched nothing.
    assert!(d.mem.stat(&vp("mem:///a")).await.is_ok());
    assert_eq!(read_all(&d.mem, "mem:///z").await, b"intruso");
}

/// The pair cap is enforced AT THE BOUNDARY and REJECTS (does not trim): a
/// trimmed batch would run a plan different from the one requested. BOTH
/// methods.
#[tokio::test]
async fn rename_batch_above_the_pair_cap_is_invalid_params() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let too_many: Vec<methods::RenamePair> = (0..=methods::FS_RENAME_BATCH_MAX_PAIRS)
        .map(|i| pair(format!("f{i}").as_bytes(), format!("g{i}").as_bytes()))
        .collect();
    assert_eq!(too_many.len(), methods::FS_RENAME_BATCH_MAX_PAIRS + 1);

    let err = c
        .call::<_, methods::FsRenameBatchPlanResult>(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///"),
                pairs: too_many.clone(),
            },
        )
        .await
        .expect_err("above the cap");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::InvalidPath)),
            "InvalidPath, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }

    let err = c
        .call::<_, FsTaskResult>(
            methods::FS_RENAME_BATCH,
            &methods::FsRenameBatchParams {
                dir: vp("mem:///"),
                pairs: too_many,
                plan_hash: methods::PlanHash::parse(&"0".repeat(64)).expect("hash"),
            },
        )
        .await
        .expect_err("above the cap");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(Error::InvalidPath)),
            "InvalidPath, was {:?}",
            rpc.data
        ),
        other => panic!("expected Rpc, got {other:?}"),
    }

    // Right at the cap it is NOT a params error (it dies for another reason,
    // or passes): the rejection is about the EXCESS, not the legal size.
    let at_cap: Vec<methods::RenamePair> = (0..methods::FS_RENAME_BATCH_MAX_PAIRS)
        .map(|i| pair(format!("f{i}").as_bytes(), format!("g{i}").as_bytes()))
        .collect();
    let plan: methods::FsRenameBatchPlanResult = c
        .call(
            methods::FS_RENAME_BATCH_PLAN,
            &methods::FsRenameBatchPlanParams {
                dir: vp("mem:///"),
                pairs: at_cap,
            },
        )
        .await
        .expect("the exact cap gets planned");
    assert!(!plan.executable, "none of those files exist");
}
