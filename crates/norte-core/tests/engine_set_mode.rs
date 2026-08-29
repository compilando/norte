//! Integración `Engine::set_mode` (#314): cambiar permisos POSIX es una
//! MUTACIÓN, con todo lo que eso arrastra — journal con reversa, política, y
//! una Task cancelable.
//!
//! `MemProvider` in-memory → determinista, sin tocar disco. Publica
//! `posix.mode` y lo escribe, que es lo que hace comprobable el undo.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::journal::Actor;
use norte_core::{Engine, Journal, SqliteJournal, UndoReport};
use norte_proto::methods::FsSetModeParams;
use norte_proto::{Error as ProtoError, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

async fn write_file(mem: &MemProvider, wire: &str) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::from_static(b"x")).await.expect("chunk");
    sink.commit().await.expect("commit");
}

async fn setup() -> (Engine, Arc<MemProvider>, Arc<SqliteJournal>) {
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal"),
    ));
    let engine = Engine::with_journal(Arc::clone(&journal));
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem, journal)
}

/// El modo que `p` tiene AHORA, leído por el atributo que el provider publica.
async fn modo(mem: &MemProvider, wire: &str) -> u32 {
    let req = norte_vfs::AttrRequest::sanitized(vec!["posix.mode".to_owned()]);
    let opt = norte_vfs::ListOptions { attrs: req };
    let e = mem.stat_with(&vp(wire), &opt).await.expect("stat");
    match e.attrs.get("posix.mode").expect("el provider lo publica") {
        norte_proto::AttrValue::Uint(m) => u32::try_from(*m).expect("cabe"),
        otro => panic!("posix.mode no es un uint: {otro:?}"),
    }
}

fn params(paths: &[&str], mode: u32) -> FsSetModeParams {
    FsSetModeParams {
        paths: paths.iter().map(|p| vp(p)).collect(),
        mode,
    }
}

async fn run_undo(engine: &Engine, actor: Actor) -> (TaskState, UndoReport) {
    let (h, report) = engine.undo_session(actor).await.expect("undo submit");
    let state = h.join().await;
    let r = report.lock().expect("lock").clone();
    (state, r)
}

/// Lo básico: cambia el modo de un lote, y el listado lo dice.
#[tokio::test]
async fn cambia_el_modo_de_un_lote() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///a.sh").await;
    write_file(&mem, "mem:///b.sh").await;

    let h = engine
        .set_mode(params(&["mem:///a.sh", "mem:///b.sh"], 0o755))
        .await
        .expect("lanza");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(modo(&mem, "mem:///a.sh").await, 0o755);
    assert_eq!(modo(&mem, "mem:///b.sh").await, 0o755);
}

/// **La reversa es el modo ANTERIOR**, y deshacer lo devuelve. Sin esto, un
/// cambio de permisos sería la única mutación de norte sin vuelta atrás, y no
/// hay ninguna razón para que lo sea: los doce bits de antes caben en el
/// journal.
#[tokio::test]
async fn deshacer_devuelve_los_permisos_de_antes() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///a.sh").await;
    let antes = modo(&mem, "mem:///a.sh").await;

    let h = engine
        .set_mode(params(&["mem:///a.sh"], 0o700))
        .await
        .expect("lanza");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(modo(&mem, "mem:///a.sh").await, 0o700);

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 1);
    assert_eq!(
        modo(&mem, "mem:///a.sh").await,
        antes,
        "el undo devuelve el modo que tenía, no uno inventado"
    );
}

/// Un lote deja UNA entrada por ruta, así que deshacer un lote de tres las
/// deshace las tres — y no «el lote», que no existe como cosa.
#[tokio::test]
async fn un_lote_deja_una_entrada_por_ruta() {
    let (engine, mem, _j) = setup().await;
    for n in ["a", "b", "c"] {
        write_file(&mem, &format!("mem:///{n}.sh")).await;
    }
    let h = engine
        .set_mode(params(
            &["mem:///a.sh", "mem:///b.sh", "mem:///c.sh"],
            0o750,
        ))
        .await
        .expect("lanza");
    assert_eq!(h.join().await, TaskState::Completed);

    let (state, r) = run_undo(&engine, Actor::User).await;
    assert_eq!(state, TaskState::Completed);
    assert_eq!(r.undone, 3, "una entrada por ruta, y las tres se deshacen");
    assert_eq!(modo(&mem, "mem:///a.sh").await, 0o644);
    assert_eq!(modo(&mem, "mem:///c.sh").await, 0o644);
}

/// Regla 3: se cancela limpiamente y el estado lo dice.
#[tokio::test]
async fn cambiar_permisos_se_cancela_y_lo_dice() {
    let (engine, mem, _j) = setup().await;
    let mut rutas = Vec::new();
    for i in 0..400 {
        let wire = format!("mem:///f{i}");
        write_file(&mem, &wire).await;
        rutas.push(wire);
    }
    let refs: Vec<&str> = rutas.iter().map(String::as_str).collect();
    let h = engine.set_mode(params(&refs, 0o600)).await.expect("lanza");
    h.cancel();
    assert_eq!(h.join().await, TaskState::Cancelled);
}

/// Sin rutas no hay petición, y se rechaza ANTES de crear Task alguna: es un
/// error del REQUEST, no el fallo de algo ya lanzado.
#[tokio::test]
async fn sin_rutas_no_se_crea_task() {
    let (engine, _mem, _j) = setup().await;
    let Err(err) = engine.set_mode(params(&[], 0o644)).await else {
        panic!("cambiarle el modo a nada no es una petición");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "{err:?}");
}

/// Los bits que NO son de permiso dicen de qué clase es el nodo, y eso no se
/// cambia. Se rechazan en vez de recortarse: recortar dejaría un permiso que
/// nadie pidió, y encima con cara de haber obedecido.
#[tokio::test]
async fn un_modo_con_bits_de_clase_se_rechaza() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///a.sh").await;
    // 0o100644: fichero regular + 644. El de arriba es el que sobra.
    let Err(err) = engine.set_mode(params(&["mem:///a.sh"], 0o100_644)).await else {
        panic!("los bits de clase no son un permiso que fijar");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "{err:?}");
    assert_eq!(
        modo(&mem, "mem:///a.sh").await,
        0o644,
        "y no se ha tocado nada"
    );
}

/// Por encima del tope se RECHAZA, no se recorta: media selección con los
/// permisos de antes y sin decir cuál es lo que este rechazo evita.
#[tokio::test]
async fn por_encima_del_tope_se_rechaza() {
    let (engine, _mem, _j) = setup().await;
    let n = norte_proto::methods::FS_SET_MODE_MAX_PATHS + 1;
    let muchas: Vec<VPath> = (0..n).map(|i| vp(&format!("mem:///f{i}"))).collect();
    let Err(err) = engine
        .set_mode(FsSetModeParams {
            paths: muchas,
            mode: 0o644,
        })
        .await
    else {
        panic!("por encima del tope tiene que rechazarse");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "{err:?}");
}

/// **setuid y setgid, solo a mano** (ADR 0081).
///
/// No porque esos bits sean el peligro —un `chmod 0777` sobre `~/.ssh` hace
/// mucho más daño y no lleva ninguno—, sino porque son los que quien aprueba
/// NO PUEDE VER: la petición de aprobación lleva la op y las rutas, no el
/// modo. El humano sí los fija, desde un diálogo que sí los enseña.
#[tokio::test]
async fn setuid_y_setgid_no_los_pone_un_agente() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///a.sh").await;
    let agente = Actor::Agent {
        session: "s1".into(),
    };
    for especial in [0o4755, 0o2755, 0o6755] {
        let Err(err) = engine
            .set_mode_as(params(&["mem:///a.sh"], especial), agente.clone())
            .await
        else {
            panic!("{especial:o} lo tiene que rehusar para un agente");
        };
        assert!(matches!(err, ProtoError::PolicyDenied { .. }), "{err:?}");
    }
    assert_eq!(modo(&mem, "mem:///a.sh").await, 0o644, "y no tocó nada");

    // El sticky (0o1000) NO entra en esa cuenta: no otorga privilegio de
    // nadie, y en un directorio es lo que hace que `/tmp` funcione.
    let h = engine
        .set_mode_as(params(&["mem:///a.sh"], 0o1755), agente)
        .await
        .expect("el sticky no es un bit de privilegio");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(modo(&mem, "mem:///a.sh").await, 0o1755);
}

/// **Un SYMLINK no se toca**, y esto no es remilgo.
///
/// `chmod(2)` SIGUE el enlace mientras que el `stat` con el que se lee la
/// reversa NO lo sigue (lstat, contrato del trait). Así que el modo guardado
/// como «el de antes» sería el del ENLACE —`0o777` siempre en Linux— y
/// deshacer dejaría el DESTINO abierto a todo el mundo. Y hay algo peor que la
/// reversa: el destino puede estar fuera del scope que alguien aprobó, así que
/// un chmod sobre un enlace es una escritura que se sale de su raíz.
#[tokio::test]
async fn un_symlink_no_se_toca() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///real.txt").await;
    mem.symlink(
        &vp("mem:///enlace"),
        b"real.txt",
        norte_vfs::SymlinkKind::File,
    )
    .await
    .expect("enlace");

    let h = engine
        .set_mode(params(&["mem:///enlace"], 0o777))
        .await
        .expect("lanza");
    let prog = h.progress();
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        modo(&mem, "mem:///real.txt").await,
        0o644,
        "el destino del enlace conserva sus permisos"
    );
    // Y el progreso lo cuenta como no hecha, que es lo que el frontend dice.
    assert_eq!(prog.borrow().unreadable, Some(1));
}

/// Una ruta que falla no tumba el lote: la selección de cincuenta no se pierde
/// por el fichero que ya no está.
#[tokio::test]
async fn una_ruta_que_falla_no_tumba_el_lote() {
    let (engine, mem, _j) = setup().await;
    write_file(&mem, "mem:///buena.sh").await;
    let h = engine
        .set_mode(params(&["mem:///no-existe", "mem:///buena.sh"], 0o700))
        .await
        .expect("lanza");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(modo(&mem, "mem:///buena.sh").await, 0o700);
}
