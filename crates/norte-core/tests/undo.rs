//! Integración del undo de sesión (M3-2): `Engine::undo_session` deshace en
//! LIFO, estricto (nunca pisa), con compensaciones append-only.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::journal::Actor;
use norte_core::{Engine, Journal, Reversal, SqliteJournal, UndoReport};
use norte_proto::{DeleteMode, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(w: &str) -> VPath {
    VPath::parse(w).expect("wire")
}

async fn write_file(mem: &MemProvider, w: &str, c: &[u8]) {
    let mut s = mem.write(&vp(w)).await.expect("open");
    s.write(Bytes::copy_from_slice(c)).await.expect("chunk");
    s.commit().await.expect("commit");
}

async fn setup() -> (Engine, Arc<MemProvider>, Arc<SqliteJournal>) {
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("j"),
    ));
    let engine = Engine::with_journal(Arc::clone(&journal));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem, journal)
}

async fn run_undo(engine: &Engine, actor: Actor) -> (TaskState, UndoReport) {
    let (h, report) = engine.undo_session(actor).await.expect("undo submit");
    let state = h.join().await;
    let r = report.lock().expect("lock").clone();
    (state, r)
}

/// **Deshacer un cambio de ORTOGRAFÍA vuelve al nombre de antes** (#274).
///
/// El origen «está ocupado» siempre: en el volumen que pliega —el único donde
/// ese rename ocurre— `stat("Foo.txt")` encuentra el `foo.txt` que se acaba de
/// crear, así que la comprobación de «libre» decía que no y el undo se
/// bloqueaba de forma garantizada. Y un `Blocked` estrangula el LIFO: deja
/// varado todo lo anterior de la sesión.
///
/// Lo que desempata es la identidad: lo que ocupa el origen ES el nodo que se
/// está devolviendo.
#[tokio::test]
async fn deshacer_un_cambio_de_ortografia_vuelve_al_nombre_de_antes() {
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("j"),
    ));
    let engine = Engine::with_journal(Arc::clone(&journal));
    let mem = Arc::new(
        MemProvider::with_flags(
            norte_proto::CapabilityFlags::RENAME_ATOMIC
                | norte_proto::CapabilityFlags::CASE_PRESERVING,
        )
        .with_folding_noreplace(),
    );
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    write_file(&mem, "mem:///Foo.txt", b"hola").await;

    let h = engine
        .move_(&vp("mem:///Foo.txt"), &vp("mem:///foo.txt"))
        .await
        .expect("move");
    assert_eq!(h.join().await, TaskState::Completed);

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 1, "y no BLOQUEADO: {r:?}");
    assert!(
        mem.stat(&vp("mem:///Foo.txt")).await.is_ok(),
        "vuelve a llamarse como se llamaba"
    );
    let quedan: Vec<Vec<u8>> = {
        let mut s = mem.list(&vp("mem:///")).await.expect("lista");
        let mut out = Vec::new();
        while let Some(e) = s.next().await {
            out.push(
                e.expect("entrada")
                    .path
                    .file_name()
                    .expect("hoja")
                    .as_bytes()
                    .to_vec(),
            );
        }
        out
    };
    assert!(
        !quedan.iter().any(|n| n.starts_with(b".norte-rename-")),
        "sin residuo del rodeo: {quedan:?}"
    );
}

/// **`fs.create` crea un fichero VACÍO, y su deshacer lo borra** (#290).
///
/// Es una mutación como cualquier otra: journal `Created` con su reversa. Lo
/// que este test clava es que no hay excepción por ser pequeña.
#[tokio::test]
async fn crear_un_fichero_vacio_se_deshace() {
    let (engine, mem, _j) = setup().await;
    let h = engine
        .create_file(&vp("mem:///nuevo.txt"))
        .await
        .expect("create");
    assert_eq!(h.join().await, TaskState::Completed);
    let e = mem.stat(&vp("mem:///nuevo.txt")).await.expect("existe");
    assert_eq!(e.size, Some(0), "y VACÍO: crear no inventa contenido");

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 1);
    assert!(matches!(
        mem.stat(&vp("mem:///nuevo.txt")).await,
        Err(norte_proto::Error::NotFound)
    ));
}

/// Y NUNCA pisa lo que haya.
///
/// No hay ninguna lectura de «crear» que signifique «vaciar lo que hay», y un
/// método que trunca en silencio es una pérdida de datos con nombre inocente.
#[tokio::test]
async fn crear_sobre_algo_que_existe_falla_sin_tocarlo() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///ocupado.txt", b"contenido que importa").await;
    let h = engine
        .create_file(&vp("mem:///ocupado.txt"))
        .await
        .expect("submit");
    assert!(
        matches!(
            h.join().await,
            TaskState::Failed {
                error: norte_proto::Error::Conflict { .. }
            }
        ),
        "un nombre ocupado es un conflicto"
    );
    let e = mem.stat(&vp("mem:///ocupado.txt")).await.expect("sigue");
    assert_eq!(
        e.size,
        Some(21),
        "y con sus bytes intactos: el fallo no vació nada"
    );
}

#[tokio::test]
async fn undo_deletes_created() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///src.txt", b"x").await;
    let h = engine
        .copy(&vp("mem:///src.txt"), &vp("mem:///dst.txt"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);
    assert!(mem.stat(&vp("mem:///dst.txt")).await.is_ok());

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 1);
    assert!(r.blocked.is_none());
    assert!(matches!(
        mem.stat(&vp("mem:///dst.txt")).await,
        Err(norte_proto::Error::NotFound)
    ));
}

#[tokio::test]
async fn undo_restores_renamed() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///a.txt", b"x").await;
    let h = engine
        .move_(&vp("mem:///a.txt"), &vp("mem:///b.txt"))
        .await
        .expect("move");
    assert_eq!(h.join().await, TaskState::Completed);

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 1);
    assert!(
        mem.stat(&vp("mem:///a.txt")).await.is_ok(),
        "origen restaurado"
    );
    assert!(matches!(
        mem.stat(&vp("mem:///b.txt")).await,
        Err(norte_proto::Error::NotFound)
    ));
}

#[tokio::test]
async fn undo_lifo_reverts_all_then_double_undo_is_noop() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///s.txt", b"x").await;
    for d in ["mem:///a", "mem:///b", "mem:///c"] {
        let h = engine
            .copy(&vp("mem:///s.txt"), &vp(d))
            .await
            .expect("copy");
        assert_eq!(h.join().await, TaskState::Completed);
    }
    let (_s, r1) = run_undo(&engine, Actor::User).await;
    assert_eq!(r1.undone, 3);
    for d in ["mem:///a", "mem:///b", "mem:///c"] {
        assert!(matches!(
            mem.stat(&vp(d)).await,
            Err(norte_proto::Error::NotFound)
        ));
    }
    // 2º undo: todo ya compensado → 0.
    let (_s2, r2) = run_undo(&engine, Actor::User).await;
    assert_eq!(r2.undone, 0);
}

#[tokio::test]
async fn undo_blocks_when_target_occupied_by_foreign_state() {
    let (engine, mem, journal) = setup().await;
    let agent = Actor::Agent {
        session: "s1".into(),
    };
    // Estado FS coherente con un Renamed a→b del agente: b existe, a no.
    write_file(&mem, "mem:///b.txt", b"x").await;
    journal
        .journal()
        .record(
            "renamed",
            b"mem:///b.txt",
            Some(b"mem:///a.txt"),
            Reversal::RenameBack,
            None,
            &agent,
        )
        .await
        .expect("seed");
    // Drift: alguien ocupa el origen `a` (NO en la sesión del agente).
    write_file(&mem, "mem:///a.txt", b"ocupado").await;

    let (state, r) = run_undo(&engine, agent).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 0);
    assert!(
        matches!(r.blocked, Some((_, norte_proto::Error::Conflict { .. }))),
        "origen ocupado bloquea, {:?}",
        r.blocked
    );
    // No pisó: ambos intactos.
    assert!(mem.stat(&vp("mem:///a.txt")).await.is_ok());
    assert!(mem.stat(&vp("mem:///b.txt")).await.is_ok());
}

#[tokio::test]
async fn undo_skips_irreversible_permanent_delete() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///keep.txt", b"x").await;
    write_file(&mem, "mem:///gone.txt", b"x").await;
    let h = engine
        .copy(&vp("mem:///keep.txt"), &vp("mem:///copy.txt"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);
    let h = engine.delete(&vp("mem:///gone.txt")).await.expect("delete");
    assert_eq!(h.join().await, TaskState::Completed);

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 1, "deshace la copia");
    assert_eq!(r.skipped_irreversible, 1, "salta el borrado permanente");
    assert!(r.blocked.is_none(), "irreversible NO bloquea");
    assert!(matches!(
        mem.stat(&vp("mem:///copy.txt")).await,
        Err(norte_proto::Error::NotFound)
    ));
}

#[tokio::test]
async fn undo_filters_by_actor() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///u.txt", b"x").await;
    let h = engine
        .copy(&vp("mem:///u.txt"), &vp("mem:///user_copy.txt"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);
    // Created del agente (sembrado; FS coherente).
    write_file(&mem, "mem:///agent_made.txt", b"x").await;
    let agent = Actor::Agent {
        session: "s1".into(),
    };
    journal
        .journal()
        .record(
            "created",
            b"mem:///agent_made.txt",
            None,
            Reversal::Delete,
            None,
            &agent,
        )
        .await
        .expect("seed");

    let (_s, r) = run_undo(&engine, agent).await;
    assert_eq!(r.undone, 1);
    assert!(matches!(
        mem.stat(&vp("mem:///agent_made.txt")).await,
        Err(norte_proto::Error::NotFound)
    ));
    assert!(
        mem.stat(&vp("mem:///user_copy.txt")).await.is_ok(),
        "no tocó al User"
    );
}

#[tokio::test]
async fn undo_trashed_logical_restores_from_dest() {
    // Papelera lógica sembrada: un Trashed con reversal_ref = ruta del payload.
    let (engine, mem, journal) = setup().await;
    // FS coherente: el original NO existe; el payload en la papelera SÍ.
    mem.mkdir(&vp("mem:///.trash")).await.expect("mkdir trash");
    mem.mkdir(&vp("mem:///.trash/1")).await.expect("mkdir id");
    write_file(&mem, "mem:///.trash/1/v.txt", b"payload").await;
    journal
        .journal()
        .record(
            "trashed",
            b"mem:///v.txt",
            None,
            Reversal::RestoreTrash,
            Some(b"mem:///.trash/1/v.txt"),
            &Actor::User,
        )
        .await
        .expect("seed");

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 1);
    assert_eq!(
        // Restaurado al original desde el payload.
        {
            let mut s = mem.read(&vp("mem:///v.txt"), None).await.expect("read");
            let mut out = Vec::new();
            while let Some(c) = s.next().await {
                out.extend_from_slice(&c.expect("chunk"));
            }
            out
        },
        b"payload"
    );
    assert!(
        matches!(
            mem.stat(&vp("mem:///.trash/1/v.txt")).await,
            Err(norte_proto::Error::NotFound)
        ),
        "el payload se movió fuera de la papelera"
    );
}

#[tokio::test]
async fn undo_cancellation_is_clean() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///s.txt", b"x").await;
    // 4 Created para tener trabajo que cancelar a mitad.
    for d in ["mem:///a", "mem:///b", "mem:///c", "mem:///d"] {
        let h = engine
            .copy(&vp("mem:///s.txt"), &vp(d))
            .await
            .expect("copy");
        assert_eq!(h.join().await, TaskState::Completed);
    }
    // Latencia por op → ventana determinista para cancelar antes de terminar.
    mem.faults()
        // No se puede pausar el reloj aquí: el journal sqlx agota el pool
        // (`PoolTimedOut`) cuando tokio adelanta el tiempo. Ventana ancha en
        // su lugar: 200 ms por op frente a 15 ms de espera, 50x de margen.
        .set_latency_per_op(Some(std::time::Duration::from_millis(200)));

    let (h, report) = engine.undo_session(Actor::User).await.expect("submit");
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    h.cancel();
    let state = h.join().await;

    assert_eq!(state, TaskState::Cancelled, "corte cooperativo limpio");
    let r = report.lock().expect("lock").clone();
    assert!(
        r.undone < 4,
        "cancelada antes de terminar (undone={})",
        r.undone
    );
    assert!(r.blocked.is_none(), "cancelación no es bloqueo");
    // Coherencia: el corte es ENTRE entradas (cada paso es op+compensación por
    // entrada), nunca a mitad de una — la cadena sigue íntegra.
    mem.faults().set_latency_per_op(None);
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact(),
        "hash-chain íntegra tras cancelar"
    );
}

#[tokio::test]
async fn undo_without_journal_is_unsupported() {
    let engine = Engine::new(); // observer no-op, sin journal
    let err = engine
        .undo_session(Actor::User)
        .await
        .err()
        .expect("sin journal");
    assert!(matches!(err, norte_proto::Error::Unsupported));
}

#[tokio::test]
async fn undo_delete_mode_trash_then_restore_via_engine() {
    // MemProvider trashea con "vanish" (dest=None) → restore_trashed default
    // Unsupported → el undo BLOQUEA limpio (no hay papelera nativa que consultar).
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///t.txt", b"x").await;
    let h = engine
        .delete_with(&vp("mem:///t.txt"), DeleteMode::Trash)
        .await
        .expect("trash");
    assert_eq!(h.join().await, TaskState::Completed);

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 0);
    assert!(
        matches!(r.blocked, Some((_, norte_proto::Error::Unsupported))),
        "MemProvider vanish: sin restore nativo, bloquea, {:?}",
        r.blocked
    );
}

/// M3-4 T2: un HUMANO deshace la sesión de un agente cuyo scope ya no existe
/// (expiró / nunca se renovó). El target selecciona las entradas; el EJECUTOR
/// (User, allow-all) pasa el gate y firma las compensaciones — sin el split,
/// el undo moría en `out-of-scope` del propio agente.
#[tokio::test]
async fn undo_de_sesion_de_agente_ejecutado_por_humano() {
    use norte_core::approval::DenyAll;
    use norte_core::{PolicyConfig, ScopeRegistry, ScopedPolicy};
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("j"),
    ));
    // Policy REAL instalada y registro de scopes VACÍO: el agente está fuera
    // de todo scope (como tras expirar su TTL).
    let engine = Engine::with_journal(Arc::clone(&journal)).with_policy(
        Arc::new(ScopedPolicy::new(
            ScopeRegistry::new(),
            PolicyConfig::default(),
        )),
        Arc::new(DenyAll),
    );
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);

    // Mutación del agente sembrada (patrón de undo_filters_by_actor).
    write_file(&mem, "mem:///agent_made.txt", b"x").await;
    let agent = Actor::Agent {
        session: "s1".into(),
    };
    journal
        .journal()
        .record(
            "created",
            b"mem:///agent_made.txt",
            None,
            Reversal::Delete,
            None,
            &agent,
        )
        .await
        .expect("seed");

    // Sanity: el agente sin scope NO puede deshacerse a sí mismo. Desde #171
    // eso es una fila de `denied` y no un `blocked` —la policy se pregunta
    // unidad a unidad, dentro de la Task— pero el efecto sobre el árbol es el
    // mismo: no se toca nada.
    let (h, report) = engine
        .undo_session(agent.clone())
        .await
        .expect("submit self-undo");
    let _ = h.join().await;
    let r = report.lock().expect("lock").clone();
    assert_eq!(r.denied_total, 1, "out-of-scope deniega el self-undo");
    assert!(r.blocked.is_none(), "y no es un bloqueo por drift");
    assert_eq!(r.undone, 0);
    assert!(mem.stat(&vp("mem:///agent_made.txt")).await.is_ok());

    // El humano deshace la sesión del agente: target=agente, ejecutor=User.
    let (h, report) = engine
        .undo_session_for(&agent, Actor::User)
        .await
        .expect("submit undo humano");
    assert_eq!(h.join().await, TaskState::Completed);
    let r = report.lock().expect("lock").clone();
    assert_eq!(r.undone, 1);
    assert!(r.blocked.is_none());
    assert!(matches!(
        mem.stat(&vp("mem:///agent_made.txt")).await,
        Err(norte_proto::Error::NotFound)
    ));

    // La compensación la firma el EJECUTOR (User), no el agente.
    let entries = journal.journal().entries().await.expect("entries");
    let comp = entries
        .iter()
        .find(|e| e.undoes_seq.is_some())
        .expect("hay compensatoria");
    assert_eq!(comp.actor_kind, "user", "la firma el ejecutor humano");
}

/// #65: la reversa de un `Created` en un provider SIN cap `TRASH` (sftp/object
/// con `logical_trash` OFF — el caso común remoto) NO cae a borrado permanente:
/// se SALTA con contador propio y el nodo se queda. Sin `node_id` en `Created`,
/// «lo que hoy vive en ese path» puede ser trabajo del humano posterior a la
/// creación; el undo jamás lo destruye de forma irrecuperable.
#[tokio::test]
async fn undo_created_sin_trash_se_salta_no_borra_permanente() {
    use norte_proto::CapabilityFlags;
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("j"),
    ));
    let engine = Engine::with_journal(Arc::clone(&journal));
    // Como MemProvider::new() pero SIN TRASH.
    let mem = Arc::new(MemProvider::with_flags(
        CapabilityFlags::RENAME_ATOMIC
            | CapabilityFlags::CASE_SENSITIVE
            | CapabilityFlags::CASE_PRESERVING
            | CapabilityFlags::SYMLINKS,
    ));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);

    write_file(&mem, "mem:///src.txt", b"x").await;
    let h = engine
        .copy(&vp("mem:///src.txt"), &vp("mem:///dst.txt"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 0);
    assert_eq!(
        r.skipped_created_no_trash, 1,
        "la reversa permanente se salta y se cuenta"
    );
    assert!(r.blocked.is_none(), "saltar no es bloquear: el LIFO sigue");
    assert!(
        mem.stat(&vp("mem:///dst.txt")).await.is_ok(),
        "el nodo creado SIGUE: jamás borrado permanente por undo"
    );
}

/// El orden importa (#65): un DRIFT (el nodo creado ya no está) bloquea
/// SIEMPRE, incluso sin cap `TRASH` — clasificarlo como skip tragaría la
/// señal de divergencia del modo estricto.
#[tokio::test]
async fn undo_created_sin_trash_con_drift_bloquea() {
    use norte_proto::CapabilityFlags;
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("j"),
    ));
    let engine = Engine::with_journal(Arc::clone(&journal));
    let mem = Arc::new(MemProvider::with_flags(
        CapabilityFlags::RENAME_ATOMIC
            | CapabilityFlags::CASE_SENSITIVE
            | CapabilityFlags::CASE_PRESERVING,
    ));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);

    write_file(&mem, "mem:///src.txt", b"x").await;
    let h = engine
        .copy(&vp("mem:///src.txt"), &vp("mem:///dst.txt"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);
    // Drift: alguien quitó el nodo por fuera del undo.
    mem.remove(&vp("mem:///dst.txt")).await.expect("remove");

    let (_state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(r.skipped_created_no_trash, 0, "drift NO es skip");
    let (_seq, err) = r.blocked.expect("bloquea en el drift");
    assert!(matches!(err, norte_proto::Error::NotFound));
}

/// **Organizar se deshace ENTERO: los ficheros vuelven y las carpetas que
/// creó desaparecen** (fase 8).
///
/// Es la propiedad que obliga a que `fs.organize` sea un método y no N
/// llamadas del cliente. Con los `fs.create` fuera del lote, deshacer
/// devolvería los ficheros y dejaría un árbol de directorios vacíos que el
/// humano no hizo — y que tendría que ir borrando a mano sin saber cuáles
/// eran suyos.
#[tokio::test]
async fn organizar_se_deshace_entero_con_sus_carpetas() {
    let (engine, mem, journal) = setup().await;
    mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&mem, "mem:///d/factura.pdf", b"x").await;
    write_file(&mem, "mem:///d/nota.txt", b"y").await;

    let moves = vec![
        norte_proto::methods::OrganizeMove {
            current: "factura.pdf".to_owned(),
            proposed_rel: "facturas/2026/marzo.pdf".to_owned(),
        },
        norte_proto::methods::OrganizeMove {
            current: "nota.txt".to_owned(),
            proposed_rel: "notas/nota.txt".to_owned(),
        },
    ];
    let dir = vp("mem:///d");
    let plan = norte_core::organize::OrganizePlan::bind(&dir, &moves).expect("plan válido");
    let h = engine
        .organize(&dir, &moves, plan.hash(), Actor::User)
        .await
        .expect("organize");
    assert_eq!(h.join().await, TaskState::Completed);

    assert!(
        mem.stat(&vp("mem:///d/facturas/2026/marzo.pdf"))
            .await
            .is_ok()
    );
    assert!(mem.stat(&vp("mem:///d/notas/nota.txt")).await.is_ok());
    assert!(matches!(
        mem.stat(&vp("mem:///d/factura.pdf")).await,
        Err(norte_proto::Error::NotFound)
    ));

    // Y ahora entero para atrás.
    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert!(
        r.blocked.is_none(),
        "nada debería bloquear: {:?}",
        r.blocked
    );

    assert!(
        mem.stat(&vp("mem:///d/factura.pdf")).await.is_ok(),
        "el fichero vuelve a su sitio"
    );
    assert!(mem.stat(&vp("mem:///d/nota.txt")).await.is_ok());
    for carpeta in [
        "mem:///d/facturas/2026",
        "mem:///d/facturas",
        "mem:///d/notas",
    ] {
        assert!(
            matches!(
                mem.stat(&vp(carpeta)).await,
                Err(norte_proto::Error::NotFound)
            ),
            "la carpeta {carpeta} la creó el lote, así que el undo se la lleva"
        );
    }

    // Y las entradas del lote comparten `batch_id`: eso es lo que las hace
    // UNA unidad deshacible, y sin ello nada de lo de arriba se sostiene.
    let filas = journal
        .journal()
        .page(None, 50, Some("user"))
        .await
        .expect("page");
    let del_lote: Vec<_> = filas
        .iter()
        .filter(|e| e.entry.undoes_seq.is_none())
        .collect();
    let primero = del_lote[0].entry.batch_id;
    assert!(primero.is_some(), "el lote tiene id");
    assert!(
        del_lote.iter().all(|e| e.entry.batch_id == primero),
        "las carpetas y los movimientos van en el MISMO lote"
    );
}

/// Un destino que se sale del directorio no llega ni a intentarse: el plan
/// entero se rechaza antes de crear una sola carpeta.
#[tokio::test]
async fn organizar_rechaza_un_destino_que_se_sale() {
    let (engine, mem, _j) = setup().await;
    mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&mem, "mem:///d/a.txt", b"x").await;
    let dir = vp("mem:///d");
    let bueno = vec![norte_proto::methods::OrganizeMove {
        current: "a.txt".to_owned(),
        proposed_rel: "sub/a.txt".to_owned(),
    }];
    let plan = norte_core::organize::OrganizePlan::bind(&dir, &bueno).expect("plan");

    let malo = vec![norte_proto::methods::OrganizeMove {
        current: "a.txt".to_owned(),
        proposed_rel: "../fuera.txt".to_owned(),
    }];
    let Err(err) = engine.organize(&dir, &malo, plan.hash(), Actor::User).await else {
        panic!("un `..` no se aplica");
    };
    assert!(matches!(err, norte_proto::Error::InvalidPath));
    assert!(
        mem.stat(&vp("mem:///d/a.txt")).await.is_ok(),
        "y no se tocó nada"
    );
}

/// Un `plan_hash` que no es el del plan que se revisó se rechaza: lo que se
/// aplica tiene que ser lo que un humano leyó.
#[tokio::test]
async fn organizar_rechaza_un_plan_que_no_es_el_revisado() {
    let (engine, mem, _j) = setup().await;
    mem.mkdir(&vp("mem:///d")).await.expect("mkdir");
    write_file(&mem, "mem:///d/a.txt", b"x").await;
    let dir = vp("mem:///d");
    let revisado = vec![norte_proto::methods::OrganizeMove {
        current: "a.txt".to_owned(),
        proposed_rel: "sub/a.txt".to_owned(),
    }];
    let plan = norte_core::organize::OrganizePlan::bind(&dir, &revisado).expect("plan");

    // El humano aprobó «a sub/», y lo que llega mueve a otro sitio.
    let otro = vec![norte_proto::methods::OrganizeMove {
        current: "a.txt".to_owned(),
        proposed_rel: "otro/a.txt".to_owned(),
    }];
    let Err(err) = engine.organize(&dir, &otro, plan.hash(), Actor::User).await else {
        panic!("el token es del plan que se leyó");
    };
    assert!(matches!(err, norte_proto::Error::PlanStale));
}

/// **Deshacer HASTA UN PUNTO deja intacto lo anterior** (fase 7,
/// `journal.undo_after`).
///
/// Es la propiedad de la que depende toda la línea de tiempo: el humano
/// señala una fila y dice «vuelve aquí». Si el corte se fuera una entrada
/// hacia atrás, deshacer se llevaría por delante una mutación que el humano
/// estaba mirando y quería conservar — y el journal no distingue una
/// compensación pedida de una pedida por error.
#[tokio::test]
async fn deshacer_hasta_un_punto_respeta_lo_anterior() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///uno.txt", b"1").await;
    write_file(&mem, "mem:///dos.txt", b"2").await;

    // Dos copias del humano, una detrás de otra.
    for (src, dst) in [
        ("mem:///uno.txt", "mem:///uno.copia"),
        ("mem:///dos.txt", "mem:///dos.copia"),
    ] {
        let h = engine.copy(&vp(src), &vp(dst)).await.expect("copy");
        assert_eq!(h.join().await, TaskState::Completed);
    }

    // El corte: el `seq` de la PRIMERA copia. Se conserva.
    let seq_primera = journal
        .journal()
        .page(None, 50, Some("user"))
        .await
        .expect("page")
        .last()
        .expect("hay entradas")
        .entry
        .seq;

    let (h, report) = engine.undo_after(seq_primera).await.expect("undo submit");
    assert_eq!(h.join().await, TaskState::Completed);
    let r = report.lock().expect("lock").clone();

    assert_eq!(r.undone, 1, "sólo la segunda copia");
    assert!(r.blocked.is_none());
    assert!(
        mem.stat(&vp("mem:///uno.copia")).await.is_ok(),
        "lo anterior al corte NO se toca"
    );
    assert!(
        matches!(
            mem.stat(&vp("mem:///dos.copia")).await,
            Err(norte_proto::Error::NotFound)
        ),
        "lo posterior sí"
    );
}

/// Y no se lleva lo de un AGENTE, aunque sea posterior al corte: «deshaz lo
/// mío» es lo mío. Lo de un agente se deshace por `policy.undo_session`, que
/// es otra pregunta con otra respuesta.
#[tokio::test]
async fn deshacer_hasta_un_punto_no_toca_lo_de_un_agente() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///a.txt", b"a").await;

    let h = engine
        .copy(&vp("mem:///a.txt"), &vp("mem:///humano.copia"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);
    let corte = journal
        .journal()
        .page(None, 50, Some("user"))
        .await
        .expect("page")
        .first()
        .expect("hay entradas")
        .entry
        .seq;

    // La mutación del agente se SIEMBRA en el journal con el fichero ya
    // puesto, que es como lo hacen los demás tests de este fichero: lo que se
    // comprueba es a quién mira el undo, no por qué puerta entró la entrada.
    write_file(&mem, "mem:///agente.copia", b"a").await;
    journal
        .journal()
        .record(
            "created",
            b"mem:///agente.copia",
            None,
            Reversal::Delete,
            None,
            &Actor::Agent {
                session: "s-1".into(),
            },
        )
        .await
        .expect("record del agente");

    let (h, report) = engine.undo_after(corte).await.expect("undo submit");
    assert_eq!(h.join().await, TaskState::Completed);
    let r = report.lock().expect("lock").clone();

    assert_eq!(r.undone, 0, "nada del humano después del corte");
    assert!(
        mem.stat(&vp("mem:///agente.copia")).await.is_ok(),
        "lo del agente sigue donde estaba"
    );
}
