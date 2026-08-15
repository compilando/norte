//! #164 de punta a punta y contra el sistema de ficheros REAL: un componente
//! intermedio del destino que se vuelve un symlink hacia fuera ENTRE aprobar y
//! aplicar no redirige la escritura.
//!
//! Va con `file://` a propósito. `MemProvider` no tiene symlinks intermedios que
//! seguir ni `openat` con el que negarse, así que el agujero solo existe —y solo
//! se puede demostrar cerrado— contra un filesystem de verdad.

#![cfg(unix)]

use std::sync::Arc;

use norte_core::journal::{Journal, SqliteJournal};
use norte_core::sync::{Spool, SyncPlanEvent};
use norte_core::{Actor, Engine};
use norte_proto::methods::{
    OnUnknown, PlanHash, SyncCompareOptions, SyncFailureCause, SyncMode, SyncPlanDone,
    SyncPlanParams, SyncReportResult, SyncStepKind,
};
use norte_proto::{TaskState, VPath};
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

struct Harness {
    engine: Engine,
    tree: tempfile::TempDir,
    _spool: tempfile::TempDir,
}

/// Origen, destino y un directorio FUERA del destino al que apuntar, los tres
/// bajo la raíz del provider — que es lo que hace de esto una fuga del
/// DESTINO y no un fallo de la raíz del provider, que ya se comprueba aparte.
async fn harness() -> Harness {
    let tree = tempfile::tempdir().expect("árbol");
    let spool = tempfile::tempdir().expect("spool");
    std::fs::create_dir_all(tree.path().join("s/sub")).expect("origen");
    std::fs::write(tree.path().join("s/sub/secreto.txt"), b"secreto").expect("fichero");
    std::fs::create_dir(tree.path().join("d")).expect("destino");
    std::fs::create_dir(tree.path().join("fuera")).expect("fuera");

    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal"),
    ));
    let engine = Engine::with_journal(journal);
    engine.register_provider(
        Arc::new(norte_vfs_local::LocalProvider::rooted(tree.path())) as Arc<dyn Provider>,
    );
    engine.set_spool(Spool::new(spool.path()));
    Harness {
        engine,
        tree,
        _spool: spool,
    }
}

async fn plan(h: &Harness) -> SyncPlanDone {
    let params = SyncPlanParams {
        source: vp("file:///s"),
        dest: vp("file:///d"),
        mode: SyncMode::Update,
        compare: SyncCompareOptions::default(),
        on_unknown: OnUnknown::Copy,
        include: None,
    };
    let (handle, mut rx) = h
        .engine
        .sync_plan_as(params, 1, Actor::User)
        .await
        .expect("sync.plan aceptado");
    let mut done = None;
    while let Some(event) = rx.recv().await {
        if let SyncPlanEvent::Done(d) = event {
            done = Some(d);
        }
    }
    assert_eq!(handle.join().await, TaskState::Completed);
    done.expect("el plan cerró con sync.plan_done")
}

async fn apply(h: &Harness, hash: &PlanHash) -> (TaskState, SyncReportResult) {
    let (handle, report) = h
        .engine
        .sync_apply_as(hash, 1, Actor::User)
        .await
        .expect("sync.apply aceptado");
    let state = handle.join().await;
    let report = report.lock().expect("report lock").clone();
    (state, report)
}

/// El caso de #164: el plan se aprueba contra un destino limpio y, antes de
/// aplicarlo, alguien sustituye el directorio intermedio por un symlink hacia
/// fuera. La copia NO puede aterrizar ahí.
#[tokio::test]
async fn una_sincronizacion_no_sigue_un_symlink_intermedio_fuera_de_su_destino() {
    let h = harness().await;
    let done = plan(&h).await;
    assert!(
        done.executable,
        "el plan se aprueba contra un destino limpio"
    );

    // La ventana del TTL: entre aprobar y aplicar, `d/sub` deja de ser el
    // directorio que el plan creará y pasa a ser un puente a `fuera`.
    std::os::unix::fs::symlink(h.tree.path().join("fuera"), h.tree.path().join("d/sub"))
        .expect("symlink hostil");

    let (state, report) = apply(&h, &done.plan_hash).await;

    assert_eq!(state, TaskState::Completed, "la Task termina, no revienta");
    assert!(
        !h.tree.path().join("fuera/secreto.txt").exists(),
        "no escribió fuera del destino"
    );
    // Y se contó como conflicto: la fila del informe dice que ese paso no
    // ocurrió, en vez de callarse una escritura que aterrizó en otro sitio.
    assert!(
        report
            .failures
            .iter()
            .any(|f| { f.kind == SyncStepKind::Copy && f.cause == SyncFailureCause::Conflict }),
        "la copia sale como conflicto: {:?}",
        report.failures
    );
}
