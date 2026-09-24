//! Scheduler tests: lifecycle, clean cancellation (hard rule 3), supervised
//! panics, priority, concurrency limit and coalescing.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use norte_core::{Actor, Priority, Scheduler, TaskBody};
use norte_proto::{Error, TaskKind, TaskState};

fn body(
    f: impl FnOnce(norte_core::TaskCtx) -> futures::future::BoxFuture<'static, Result<(), Error>>
    + Send
    + 'static,
) -> TaskBody {
    Box::new(f)
}

#[tokio::test]
async fn task_completes_and_reports_progress() {
    let sched = Scheduler::new(2);
    let handle = sched.submit(
        "mem",
        TaskKind::Copy,
        Priority::Normal,
        Actor::User,
        body(|ctx| {
            Box::pin(async move {
                ctx.progress.update(|p| {
                    p.bytes_total = Some(10);
                    p.bytes_done = 10;
                });
                Ok(())
            })
        }),
    );
    let final_state = handle.join().await;
    assert_eq!(final_state, TaskState::Completed);
}

#[tokio::test]
async fn cancellation_is_clean_and_cooperative() {
    let sched = Scheduler::new(2);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = sched.submit(
        "mem",
        TaskKind::Delete,
        Priority::Normal,
        Actor::User,
        body(move |ctx| {
            Box::pin(async move {
                let _ = started_tx.send(());
                // Inner loop with a cancellation check (hard rule 3).
                loop {
                    if ctx.cancel.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    tokio::task::yield_now().await;
                }
            })
        }),
    );
    started_rx.await.expect("the task started");
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);
}

#[tokio::test]
async fn panic_is_supervised_and_scheduler_survives() {
    let sched = Scheduler::new(2);
    let handle = sched.submit(
        "mem",
        TaskKind::Copy,
        Priority::Normal,
        Actor::User,
        body(|_ctx| Box::pin(async { panic!("task broken on purpose") })),
    );
    match handle.join().await {
        TaskState::Failed { error } => assert_eq!(error, Error::Internal { panic: true }),
        other => panic!("expected Failed{{panic}}, was {other:?}"),
    }
    // The scheduler is still alive: another task runs fine.
    let ok = sched.submit(
        "mem",
        TaskKind::Copy,
        Priority::Normal,
        Actor::User,
        body(|_ctx| Box::pin(async { Ok(()) })),
    );
    assert_eq!(ok.join().await, TaskState::Completed);
}

#[tokio::test]
async fn error_maps_to_failed_with_taxonomy() {
    let sched = Scheduler::new(2);
    let handle = sched.submit(
        "mem",
        TaskKind::Move,
        Priority::Normal,
        Actor::User,
        body(|_ctx| Box::pin(async { Err(Error::NotFound) })),
    );
    assert_eq!(
        handle.join().await,
        TaskState::Failed {
            error: Error::NotFound
        }
    );
}

#[tokio::test]
async fn priority_orders_queued_work() {
    // 1 permit: the first task blocks; the following ones wait in the heap
    // and must come out by priority, not by arrival order.
    let sched = Scheduler::new(1);
    let order = Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();

    let blocker = sched.submit(
        "mem",
        TaskKind::Copy,
        Priority::Normal,
        Actor::User,
        body(move |_ctx| {
            Box::pin(async move {
                let _ = gate_rx.await;
                Ok(())
            })
        }),
    );
    // Queue low BEFORE high; it must run AFTER.
    let mk = |label: &'static str, order: Arc<std::sync::Mutex<Vec<&'static str>>>| {
        body(move |_ctx| {
            Box::pin(async move {
                order.lock().expect("order lock").push(label);
                Ok(())
            })
        })
    };
    let low = sched.submit(
        "mem",
        TaskKind::Copy,
        Priority::Low,
        Actor::User,
        mk("low", order.clone()),
    );
    let high = sched.submit(
        "mem",
        TaskKind::Copy,
        Priority::High,
        Actor::User,
        mk("high", order.clone()),
    );

    gate_tx.send(()).expect("unblock");
    blocker.join().await;
    high.join().await;
    low.join().await;
    assert_eq!(*order.lock().expect("order lock"), vec!["high", "low"]);
}

#[tokio::test]
async fn semaphore_bounds_concurrency_per_provider() {
    let sched = Scheduler::new(2);
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..6 {
        let live = Arc::clone(&live);
        let peak = Arc::clone(&peak);
        handles.push(sched.submit(
            "mem",
            TaskKind::Copy,
            Priority::Normal,
            Actor::User,
            body(move |_ctx| {
                Box::pin(async move {
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    live.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
        ));
    }
    for h in handles {
        assert_eq!(h.join().await, TaskState::Completed);
    }
    assert!(
        peak.load(Ordering::SeqCst) <= 2,
        "concurrency peak {} > 2",
        peak.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn providers_have_independent_queues() {
    // 1 permit per provider: an eternal task on "mem" does not block "file".
    let sched = Scheduler::new(1);
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
    let blocker = sched.submit(
        "mem",
        TaskKind::Copy,
        Priority::Normal,
        Actor::User,
        body(move |_ctx| {
            Box::pin(async move {
                let _ = gate_rx.await;
                Ok(())
            })
        }),
    );
    let other = sched.submit(
        "file",
        TaskKind::Copy,
        Priority::Normal,
        Actor::User,
        body(|_ctx| Box::pin(async { Ok(()) })),
    );
    assert_eq!(other.join().await, TaskState::Completed);
    gate_tx.send(()).expect("unblock");
    blocker.join().await;
}

#[tokio::test(start_paused = true)]
async fn progress_is_coalesced_but_terminal_always_flushes() {
    let sched = Scheduler::new(1);
    let handle = sched.submit(
        "mem",
        TaskKind::Copy,
        Priority::Normal,
        Actor::User,
        body(|ctx| {
            Box::pin(async move {
                // 100 updates without advancing the clock: they must coalesce.
                for i in 0..100u64 {
                    ctx.progress.update(|p| p.bytes_done = i);
                }
                Ok(())
            })
        }),
    );
    let mut rx = handle.progress();
    let mut published = Vec::new();
    loop {
        let snap = rx.borrow_and_update().clone();
        let terminal = snap.state.is_terminal();
        published.push(snap);
        if terminal {
            break;
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
    // The watch only keeps the last value: what is observable is that the
    // terminal one arrived and the final bytes_done is the last one (99),
    // without requiring 100 publications.
    let last = published.last().expect("at least the terminal one");
    assert_eq!(last.state, TaskState::Completed);
    assert_eq!(
        last.bytes_done, 99,
        "the terminal snapshot carries the last value"
    );
}

/// #278 — a daemon handoff (`daemon.going_away { reconnect: true }`) is
/// routine, and until now the new process handed out the SAME ids as the old
/// one: a frontend with a report in flight would land, by collision, on the
/// wrong row. The core already seeded with the clock in
/// `daemon/approvals.rs` and tasks never got that treatment.
///
/// What can be tested within a single process is tested here: the sequence
/// does NOT start at 1, and two different schedulers do not hand out the same
/// first id unless the clock has not advanced — which this test does NOT
/// assert, because separation across processes is best-effort by definition.
#[tokio::test]
async fn task_ids_do_not_start_at_one() {
    let sched = Scheduler::new(2);
    let h = sched.submit(
        "mem",
        TaskKind::Copy,
        Priority::Normal,
        Actor::User,
        body(|_| Box::pin(async { Ok(()) })),
    );
    assert!(
        h.id().get() > 1,
        "the sequence starts at 1: a new daemon repeats the old one's ids"
    );
    let _ = h.join().await;
}

/// And the ceiling: a `task_id` TRAVELS to the renderer inside `TaskView`,
/// which is JSON read by JavaScript. A nanosecond seed — what `approvals.rs`
/// uses, whose ids do NOT cross the bridge — would go past 2^53 and two
/// different ids would collapse into the same `Number`. That is worse than
/// the collision the seed fixes.
#[tokio::test]
async fn task_ids_fit_where_f64_is_exact() {
    const CAP: u64 = 1 << 53;
    let sched = Scheduler::new(2);
    let mut ids = Vec::new();
    for _ in 0..4 {
        let h = sched.submit(
            "mem",
            TaskKind::Copy,
            Priority::Normal,
            Actor::User,
            body(|_| Box::pin(async { Ok(()) })),
        );
        ids.push(h.id().get());
        let _ = h.join().await;
    }
    for id in &ids {
        assert!(*id < CAP, "id {id} is not exact as an f64 (cap 2^53)");
    }
    // And they are still consecutive: the seed shifts the origin, not the step.
    for pair in ids.windows(2) {
        assert_eq!(pair[1], pair[0] + 1);
    }
}

/// A shared buffer the test's JSON layer writes to.
#[derive(Clone, Default)]
struct Buffer(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("buffer").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// **What a task logs carries its `task` and the request that asked for it**
/// (ADR 0127).
///
/// With a single permit and crossed priorities, the runner that launched one
/// `submit` picks OTHER job off the heap: the span has to travel with the job,
/// not with the runner. And a bare `tokio::spawn` inherits nothing, so
/// without the saved span, an event from inside the task would come out with
/// no parent.
#[tokio::test]
async fn every_event_of_a_task_carries_its_span_and_the_requests() {
    use tracing_subscriber::layer::SubscriberExt as _;

    let buf = Buffer::default();
    let writer = buf.clone();
    let sub = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .json()
            .with_span_list(true)
            .with_writer(move || writer.clone()),
    );
    let _guard = tracing::subscriber::set_default(sub);

    let sched = Scheduler::new(1);
    let (open, closed) = tokio::sync::oneshot::channel::<()>();
    let task = |mark: &'static str| {
        body(move |_| {
            Box::pin(async move {
                tracing::info!(mark, "inside");
                Ok(())
            })
        })
    };

    let rpc = tracing::info_span!("rpc", method = "fs.copy");
    let (blocker, low, high) = {
        let _e = rpc.enter();
        // Occupies the only permit until the test releases it.
        let blocker = sched.submit(
            "mem",
            TaskKind::Copy,
            Priority::Normal,
            Actor::User,
            body(move |_| {
                Box::pin(async move {
                    let _ = closed.await;
                    Ok(())
                })
            }),
        );
        let low = sched.submit(
            "mem",
            TaskKind::Copy,
            Priority::Low,
            Actor::User,
            task("low"),
        );
        let high = sched.submit(
            "mem",
            TaskKind::Move,
            Priority::High,
            Actor::User,
            task("high"),
        );
        (blocker, low, high)
    };
    let expected = [
        ("low", low.id().to_string()),
        ("high", high.id().to_string()),
    ];
    let _ = open.send(());
    let _ = blocker.join().await;
    let _ = low.join().await;
    let _ = high.join().await;

    let text = String::from_utf8(buf.0.lock().expect("buffer").clone()).expect("utf-8");
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).expect("JSON line"))
        .filter(|v: &serde_json::Value| v["fields"]["message"] == "inside")
        .collect();
    assert_eq!(events.len(), 2, "one event per task: {text}");
    for v in &events {
        let mark = v["fields"]["mark"].as_str().expect("mark");
        let id = &expected
            .iter()
            .find(|(m, _)| *m == mark)
            .expect("known mark")
            .1;
        let spans = v["spans"].as_array().expect("spans");
        assert_eq!(spans[0]["name"], "rpc", "the request, outside: {v}");
        assert_eq!(spans[0]["method"], "fs.copy");
        assert_eq!(spans[1]["name"], "task", "the task, inside: {v}");
        assert_eq!(
            spans[1]["task_id"],
            id.as_str(),
            "the event for \"{mark}\" carries ITS task's id"
        );
    }
}

// ---------------------------------------------------------------------------
// Pause (ADR 0147).
// ---------------------------------------------------------------------------

/// Waits until the published progress satisfies `f`, with a safety deadline
/// that is not the wait itself: it only avoids hanging the test if it never
/// arrives.
async fn until(
    rx: &mut tokio::sync::watch::Receiver<norte_proto::TaskProgress>,
    f: impl Fn(&norte_proto::TaskProgress) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(10), rx.wait_for(|p| f(p)))
        .await
        .expect("the expected state arrived")
        .expect("the sender is still alive");
}

/// A body that advances one at a time through checkpoints until the test
/// raises `end`: finishing on its own would race with the pause.
fn counter(end: Arc<std::sync::atomic::AtomicBool>) -> TaskBody {
    body(move |ctx| {
        Box::pin(async move {
            let mut i = 0u64;
            while !end.load(Ordering::SeqCst) {
                ctx.checkpoint().await?;
                i += 1;
                ctx.progress.update(|p| p.entries_done = i);
                tokio::task::yield_now().await;
            }
            Ok(())
        })
    })
}

/// Pausing stops the task at its checkpoint and publishes it; resuming lets it
/// continue to the end.
#[tokio::test]
async fn pausing_stops_and_resuming_continues() {
    let sched = Scheduler::new(1);
    let end = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handle = sched.submit(
        "mem",
        TaskKind::Copy,
        Priority::Normal,
        Actor::User,
        counter(Arc::clone(&end)),
    );
    let mut rx = handle.progress();
    until(&mut rx, |p| p.entries_done >= 3).await;
    handle.pause_gate().pause();
    until(&mut rx, |p| p.state == TaskState::Paused).await;
    let stopped_at = rx.borrow().entries_done;
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        handle.progress().borrow().entries_done,
        stopped_at,
        "paused does not advance"
    );
    handle.pause_gate().resume();
    until(&mut rx, |p| p.entries_done > stopped_at).await;
    end.store(true, Ordering::SeqCst);
    assert_eq!(handle.join().await, TaskState::Completed);
}

/// Cancelling a PAUSED task ends it cleanly (hard rule 3): the wait listens to
/// the token.
#[tokio::test]
async fn cancelling_a_paused_one_ends_it() {
    let sched = Scheduler::new(1);
    let handle = sched.submit(
        "mem",
        TaskKind::Copy,
        Priority::Normal,
        Actor::User,
        counter(Arc::new(std::sync::atomic::AtomicBool::new(false))),
    );
    let mut rx = handle.progress();
    until(&mut rx, |p| p.entries_done >= 1).await;
    handle.pause_gate().pause();
    until(&mut rx, |p| p.state == TaskState::Paused).await;
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);
}

/// A task paused BEFORE starting does not run its body until resumed, even if
/// the body has no checkpoints of its own.
#[tokio::test]
async fn paused_before_starting_does_not_start() {
    let sched = Scheduler::new(1);
    let ran = Arc::new(AtomicUsize::new(0));
    // Occupies the only slot so the second one waits in the queue.
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let blocker = sched.submit(
        "mem",
        TaskKind::Copy,
        Priority::Normal,
        Actor::User,
        body(move |_ctx| {
            Box::pin(async move {
                let _ = release_rx.await;
                Ok(())
            })
        }),
    );
    let c = Arc::clone(&ran);
    let handle = sched.submit(
        "mem",
        TaskKind::Copy,
        Priority::Normal,
        Actor::User,
        body(move |_ctx| {
            Box::pin(async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }),
    );
    handle.pause_gate().pause();
    let _ = release_tx.send(());
    assert_eq!(blocker.join().await, TaskState::Completed);
    let mut rx = handle.progress();
    until(&mut rx, |p| p.state == TaskState::Paused).await;
    assert_eq!(ran.load(Ordering::SeqCst), 0, "the body did not run");
    handle.pause_gate().resume();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(ran.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// The serial queue lane (ADR 0149).
// ---------------------------------------------------------------------------

/// What is queued runs ONE AT A TIME, in the order it came in, while the
/// parallel lane keeps admitting several at once.
#[tokio::test]
async fn the_queue_lane_runs_one_at_a_time_and_in_order() {
    use norte_core::Lane;
    let sched = Scheduler::new(4);
    let alive = Arc::new(AtomicUsize::new(0));
    let max = Arc::new(AtomicUsize::new(0));
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut handles = Vec::new();
    for i in 0..4u64 {
        let alive = Arc::clone(&alive);
        let max = Arc::clone(&max);
        let order = Arc::clone(&order);
        handles.push(sched.submit_en(
            Lane::Cola,
            "mem",
            TaskKind::Copy,
            Priority::Normal,
            Actor::User,
            body(move |_ctx| {
                Box::pin(async move {
                    let concurrent = alive.fetch_add(1, Ordering::SeqCst) + 1;
                    max.fetch_max(concurrent, Ordering::SeqCst);
                    order.lock().expect("order").push(i);
                    tokio::task::yield_now().await;
                    alive.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
        ));
    }
    for h in handles {
        assert_eq!(h.join().await, TaskState::Completed);
    }
    assert_eq!(max.load(Ordering::SeqCst), 1, "one at a time");
    assert_eq!(*order.lock().expect("order"), vec![0, 1, 2, 3], "in order");
}

/// Moving up one that has NOT STARTED YET advances it; over one already
/// running, over the first one in the queue, or over an unknown one, there is
/// nothing to move.
#[tokio::test]
async fn moving_in_the_queue_advances_what_has_not_started() {
    use norte_core::Lane;
    let sched = Scheduler::new(4);
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    // The first one occupies the queue's only slot until the test releases it:
    // that way the other three are WAITING when it gets reordered.
    let o = Arc::clone(&order);
    let first = sched.submit_en(
        Lane::Cola,
        "mem",
        TaskKind::Copy,
        Priority::Normal,
        Actor::User,
        body(move |_ctx| {
            Box::pin(async move {
                o.lock().expect("order").push(0u64);
                let _ = release_rx.await;
                Ok(())
            })
        }),
    );
    let mut waiting = Vec::new();
    for i in 1..4u64 {
        let o = Arc::clone(&order);
        waiting.push(sched.submit_en(
            Lane::Cola,
            "mem",
            TaskKind::Copy,
            Priority::Normal,
            Actor::User,
            body(move |_ctx| {
                Box::pin(async move {
                    o.lock().expect("order").push(i);
                    Ok(())
                })
            }),
        ));
    }
    // Make sure the first one really started before reordering.
    tokio::time::timeout(
        Duration::from_secs(10),
        first.progress().wait_for(|p| p.state == TaskState::Running),
    )
    .await
    .expect("starts")
    .expect("sender alive");

    assert!(
        sched.mover_en_cola(waiting[2].id(), true),
        "the last one moves up"
    );
    assert!(
        !sched.mover_en_cola(first.id(), true),
        "the one already running is not in the queue"
    );
    assert!(
        !sched.mover_en_cola(norte_proto::TaskId::new(u64::MAX - 1), true),
        "an unknown one does not move"
    );
    let _ = release_tx.send(());
    assert_eq!(first.join().await, TaskState::Completed);
    for h in waiting {
        assert_eq!(h.join().await, TaskState::Completed);
    }
    assert_eq!(
        *order.lock().expect("order"),
        vec![0, 1, 3, 2],
        "the third one moved ahead of the second… among those waiting"
    );
}
