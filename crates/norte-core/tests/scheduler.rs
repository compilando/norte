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

/// #278 — un relevo del daemon (`daemon.going_away { reconnect: true }`) es
/// rutinario, y hasta aquí el proceso nuevo repartía los MISMOS ids que el
/// viejo: un frontend con un informe en vuelo acertaba por colisión sobre la
/// fila equivocada. El core ya sembraba con el reloj en `daemon/approvals.rs`
/// y las tasks nunca recibieron ese trato.
///
/// Aquí se prueba lo que se puede probar dentro de un proceso: la secuencia NO
/// arranca en 1, y dos schedulers distintos no reparten el mismo primer id
/// salvo que el reloj no haya avanzado — cosa que este test NO afirma, porque
/// la separación entre procesos es best-effort por definición.
#[tokio::test]
async fn los_ids_de_task_no_arrancan_en_uno() {
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
        "la secuencia arranca en 1: un daemon nuevo repite los ids del viejo"
    );
    let _ = h.join().await;
}

/// Y el techo: un `task_id` VIAJA al renderer dentro de `TaskView`, que es
/// JSON leído por JavaScript. Una semilla en nanosegundos —lo que usa
/// `approvals.rs`, cuyos ids NO cruzan el puente— pasaría de 2^53 y dos ids
/// distintos colapsarían en el mismo `Number`. Eso es peor que la colisión
/// que la semilla arregla.
#[tokio::test]
async fn los_ids_de_task_caben_donde_f64_es_exacto() {
    const TOPE: u64 = 1 << 53;
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
        assert!(*id < TOPE, "id {id} no es exacto como f64 (tope 2^53)");
    }
    // Y siguen siendo consecutivos: la semilla desplaza el origen, no el paso.
    for par in ids.windows(2) {
        assert_eq!(par[1], par[0] + 1);
    }
}

/// Un buffer compartido donde escribe la capa JSON del test.
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

/// **Lo que una tarea registra lleva su `task` y la petición que la pidió**
/// (ADR 0127).
///
/// Con un solo permiso y prioridades cruzadas, el runner que lanzó un
/// `submit` saca del heap OTRO job: el span tiene que viajar con el job, no
/// con el runner. Y `tokio::spawn` a pelo no hereda nada, así que sin el span
/// guardado un evento de dentro de la tarea salía sin padre.
#[tokio::test]
async fn cada_evento_de_una_tarea_lleva_su_span_y_el_de_la_peticion() {
    use tracing_subscriber::layer::SubscriberExt as _;

    let buf = Buffer::default();
    let escritor = buf.clone();
    let sub = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .json()
            .with_span_list(true)
            .with_writer(move || escritor.clone()),
    );
    let _guard = tracing::subscriber::set_default(sub);

    let sched = Scheduler::new(1);
    let (abrir, cerrado) = tokio::sync::oneshot::channel::<()>();
    let tarea = |marca: &'static str| {
        body(move |_| {
            Box::pin(async move {
                tracing::info!(marca, "dentro");
                Ok(())
            })
        })
    };

    let rpc = tracing::info_span!("rpc", method = "fs.copy");
    let (bloqueo, baja, alta) = {
        let _e = rpc.enter();
        // Ocupa el único permiso hasta que el test lo suelte.
        let bloqueo = sched.submit(
            "mem",
            TaskKind::Copy,
            Priority::Normal,
            Actor::User,
            body(move |_| {
                Box::pin(async move {
                    let _ = cerrado.await;
                    Ok(())
                })
            }),
        );
        let baja = sched.submit(
            "mem",
            TaskKind::Copy,
            Priority::Low,
            Actor::User,
            tarea("baja"),
        );
        let alta = sched.submit(
            "mem",
            TaskKind::Move,
            Priority::High,
            Actor::User,
            tarea("alta"),
        );
        (bloqueo, baja, alta)
    };
    let esperado = [
        ("baja", baja.id().to_string()),
        ("alta", alta.id().to_string()),
    ];
    let _ = abrir.send(());
    let _ = bloqueo.join().await;
    let _ = baja.join().await;
    let _ = alta.join().await;

    let texto = String::from_utf8(buf.0.lock().expect("buffer").clone()).expect("utf-8");
    let eventos: Vec<serde_json::Value> = texto
        .lines()
        .map(|l| serde_json::from_str(l).expect("línea JSON"))
        .filter(|v: &serde_json::Value| v["fields"]["message"] == "dentro")
        .collect();
    assert_eq!(eventos.len(), 2, "un evento por tarea: {texto}");
    for v in &eventos {
        let marca = v["fields"]["marca"].as_str().expect("marca");
        let id = &esperado
            .iter()
            .find(|(m, _)| *m == marca)
            .expect("marca conocida")
            .1;
        let spans = v["spans"].as_array().expect("spans");
        assert_eq!(spans[0]["name"], "rpc", "la petición, fuera: {v}");
        assert_eq!(spans[0]["method"], "fs.copy");
        assert_eq!(spans[1]["name"], "task", "la tarea, dentro: {v}");
        assert_eq!(
            spans[1]["task_id"],
            id.as_str(),
            "el evento de «{marca}» lleva el id de SU tarea"
        );
    }
}

// ---------------------------------------------------------------------------
// Pausa (ADR 0147).
// ---------------------------------------------------------------------------

/// Espera a que el progreso publicado cumpla `f`, con un plazo de socorro
/// que no es la espera: solo evita colgar el test si nunca llega.
async fn hasta(
    rx: &mut tokio::sync::watch::Receiver<norte_proto::TaskProgress>,
    f: impl Fn(&norte_proto::TaskProgress) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(10), rx.wait_for(|p| f(p)))
        .await
        .expect("el estado esperado llegó")
        .expect("el emisor sigue vivo");
}

/// Un cuerpo que avanza de uno en uno por puntos de control hasta que el
/// test levanta `fin`: acabar por su cuenta sería una carrera con la pausa.
fn contador(fin: Arc<std::sync::atomic::AtomicBool>) -> TaskBody {
    body(move |ctx| {
        Box::pin(async move {
            let mut i = 0u64;
            while !fin.load(Ordering::SeqCst) {
                ctx.checkpoint().await?;
                i += 1;
                ctx.progress.update(|p| p.entries_done = i);
                tokio::task::yield_now().await;
            }
            Ok(())
        })
    })
}

/// Pausar para la task en su punto de control y lo publica; reanudar la
/// deja seguir hasta el final.
#[tokio::test]
async fn pausar_para_y_reanudar_sigue() {
    let sched = Scheduler::new(1);
    let fin = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handle = sched.submit(
        "mem",
        TaskKind::Copy,
        Priority::Normal,
        Actor::User,
        contador(Arc::clone(&fin)),
    );
    let mut rx = handle.progress();
    hasta(&mut rx, |p| p.entries_done >= 3).await;
    handle.pause_gate().pause();
    hasta(&mut rx, |p| p.state == TaskState::Paused).await;
    let parada = rx.borrow().entries_done;
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        handle.progress().borrow().entries_done,
        parada,
        "pausada no avanza"
    );
    handle.pause_gate().resume();
    hasta(&mut rx, |p| p.entries_done > parada).await;
    fin.store(true, Ordering::SeqCst);
    assert_eq!(handle.join().await, TaskState::Completed);
}

/// Cancelar una task PAUSADA la termina limpia (regla dura 3): la espera
/// escucha al token.
#[tokio::test]
async fn cancelar_una_pausada_la_termina() {
    let sched = Scheduler::new(1);
    let handle = sched.submit(
        "mem",
        TaskKind::Copy,
        Priority::Normal,
        Actor::User,
        contador(Arc::new(std::sync::atomic::AtomicBool::new(false))),
    );
    let mut rx = handle.progress();
    hasta(&mut rx, |p| p.entries_done >= 1).await;
    handle.pause_gate().pause();
    hasta(&mut rx, |p| p.state == TaskState::Paused).await;
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);
}

/// Una task pausada ANTES de empezar no ejecuta su cuerpo hasta reanudarse,
/// aunque el cuerpo no tenga puntos de control propios.
#[tokio::test]
async fn pausada_antes_de_empezar_no_empieza() {
    let sched = Scheduler::new(1);
    let corrio = Arc::new(AtomicUsize::new(0));
    // Ocupa el único hueco para que la segunda espere en la cola.
    let (suelta_tx, suelta_rx) = tokio::sync::oneshot::channel::<()>();
    let bloqueo = sched.submit(
        "mem",
        TaskKind::Copy,
        Priority::Normal,
        Actor::User,
        body(move |_ctx| {
            Box::pin(async move {
                let _ = suelta_rx.await;
                Ok(())
            })
        }),
    );
    let c = Arc::clone(&corrio);
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
    let _ = suelta_tx.send(());
    assert_eq!(bloqueo.join().await, TaskState::Completed);
    let mut rx = handle.progress();
    hasta(&mut rx, |p| p.state == TaskState::Paused).await;
    assert_eq!(corrio.load(Ordering::SeqCst), 0, "el cuerpo no corrió");
    handle.pause_gate().resume();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(corrio.load(Ordering::SeqCst), 1);
}
