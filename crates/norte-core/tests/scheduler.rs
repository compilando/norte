//! Tests del scheduler: ciclo de vida, cancelación limpia (regla dura 3),
//! panics supervisados, prioridad, límite de concurrencia y coalescido.

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
                // Inner loop con chequeo de cancelación (regla dura 3).
                loop {
                    if ctx.cancel.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    tokio::task::yield_now().await;
                }
            })
        }),
    );
    started_rx.await.expect("la task arrancó");
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
        body(|_ctx| Box::pin(async { panic!("task rota a propósito") })),
    );
    match handle.join().await {
        TaskState::Failed { error } => assert_eq!(error, Error::Internal { panic: true }),
        other => panic!("esperaba Failed{{panic}}, fue {other:?}"),
    }
    // El scheduler sigue vivo: otra task corre bien.
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
    // 1 permiso: la primera task bloquea; las siguientes esperan en el heap
    // y deben salir por prioridad, no por orden de llegada.
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
    // Encolar low ANTES que high; debe ejecutarse DESPUÉS.
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

    gate_tx.send(()).expect("desbloquea");
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
        "pico de concurrencia {} > 2",
        peak.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn providers_have_independent_queues() {
    // 1 permiso por provider: una task eterna en "mem" no bloquea a "file".
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
    gate_tx.send(()).expect("desbloquea");
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
                // 100 updates sin avanzar el reloj: deben coalescer.
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
    // El watch solo retiene el último valor: lo observable es que el terminal
    // llegó y que bytes_done final es el último (99), sin exigir 100 publicaciones.
    let last = published.last().expect("al menos el terminal");
    assert_eq!(last.state, TaskState::Completed);
    assert_eq!(
        last.bytes_done, 99,
        "el snapshot terminal lleva el último dato"
    );
}
