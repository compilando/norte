//! Integración engine↔journal (M3-1b): las mutaciones del engine con un
//! `SqliteJournal` real producen las entradas esperadas, en orden y con
//! hash-chain válida. `MemProvider` in-memory → determinista, sin harness.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::{Engine, Journal, MutationObserver, SqliteJournal};
use norte_proto::{DeleteMode, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

/// Engine con journal in-memory + `MemProvider` (sus caps por defecto incluyen
/// `TRASH`). Devuelve el `SqliteJournal` para inspeccionar las entradas.
async fn setup() -> (Engine, Arc<MemProvider>, Arc<SqliteJournal>) {
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal open"),
    ));
    let engine = Engine::with_observer(Arc::clone(&journal) as Arc<dyn MutationObserver>);
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem, journal)
}

#[tokio::test]
async fn copy_records_created_with_valid_chain() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///src.txt", b"hola").await;

    let h = engine
        .copy(&vp("mem:///src.txt"), &vp("mem:///dst.txt"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);

    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].op, "created");
    assert_eq!(es[0].path, b"mem:///dst.txt");
    assert_eq!(es[0].reversal, "delete");
    assert_eq!(es[0].actor_kind, "user");
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact()
    );
}

#[tokio::test]
async fn move_records_renamed() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///a.txt", b"x").await;

    let h = engine
        .move_(&vp("mem:///a.txt"), &vp("mem:///b.txt"))
        .await
        .expect("move");
    assert_eq!(h.join().await, TaskState::Completed);

    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].op, "renamed");
    assert_eq!(es[0].path, b"mem:///b.txt");
    assert_eq!(es[0].path_to.as_deref(), Some(&b"mem:///a.txt"[..]));
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact()
    );
}

#[tokio::test]
async fn permanent_delete_records_removed_irreversible() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///gone.txt", b"x").await;

    let h = engine.delete(&vp("mem:///gone.txt")).await.expect("delete");
    assert_eq!(h.join().await, TaskState::Completed);

    let es = journal.journal().entries().await.expect("entries");
    let last = es.last().expect("entrada");
    assert_eq!(last.op, "removed");
    assert_eq!(last.reversal, "irreversible");
    assert_eq!(last.reversal_ref, None);
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact()
    );
}

#[tokio::test]
async fn trash_records_trashed_restore() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///t.txt", b"x").await;

    let h = engine
        .delete_with(&vp("mem:///t.txt"), DeleteMode::Trash)
        .await
        .expect("trash");
    assert_eq!(h.join().await, TaskState::Completed);

    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].op, "trashed");
    assert_eq!(es[0].reversal, "restore_trash");
    // MemProvider = papelera "vanish": sin ruta recuperable.
    assert_eq!(es[0].reversal_ref, None);
    assert!(
        journal
            .journal()
            .verify_chain()
            .await
            .expect("verify")
            .is_intact()
    );
}

/// #99 — la papelera NATIVA (dest `None`) tras un transitorio-tras-efecto
/// degrada a `Ok(None)` (undo sin `reversal_ref`) pero NO falla la task: el
/// ítem ya se trasheó, jamás pérdida. Antes fallaba al propagar el transitorio.
#[tokio::test]
async fn trash_nativo_transitorio_degrada_sin_fallar() {
    let (engine, mem, journal) = setup().await; // MemProvider vanish (dest None)
    write_file(&mem, "mem:///n.txt", b"x").await;

    mem.faults().ambiguous_mutations(1);
    let h = engine
        .delete_with(&vp("mem:///n.txt"), DeleteMode::Trash)
        .await
        .expect("trash");
    assert_eq!(h.join().await, TaskState::Completed);

    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es[0].op, "trashed");
    assert_eq!(
        es[0].reversal_ref, None,
        "papelera nativa: el undo degrada, la task NO falla"
    );
}

/// #99 — un trash lógico que APLICA el movimiento pero devuelve transitorio
/// no debe fallar la task ni perder el `reversal_ref`: el engine reintenta con
/// el MISMO id determinista y el provider recupera el payload. Antes (sin
/// `trash_retrying`) la task fallaba en el primer transitorio.
#[tokio::test]
async fn trash_logico_transitorio_conserva_el_reversal_ref() {
    let journal = Arc::new(SqliteJournal::new(
        Journal::open_in_memory().await.expect("journal open"),
    ));
    let engine = Engine::with_observer(Arc::clone(&journal) as Arc<dyn MutationObserver>);
    let mem = Arc::new(MemProvider::new().with_logical_trash());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    write_file(&mem, "mem:///t.txt", b"x").await;

    // El movimiento aplica y aun así devuelve transitorio (una vez).
    mem.faults().ambiguous_mutations(1);
    let h = engine
        .delete_with(&vp("mem:///t.txt"), DeleteMode::Trash)
        .await
        .expect("trash");
    assert_eq!(h.join().await, TaskState::Completed);

    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].op, "trashed");
    let comp_ref = es[0].reversal_ref.clone().expect("reversal_ref preservado");
    assert!(
        String::from_utf8_lossy(&comp_ref).contains(".norte-trash/"),
        "el payload recuperable sobrevive al transitorio: {:?}",
        String::from_utf8_lossy(&comp_ref)
    );
}

/// #32.1 — un COMMIT del write que APLICA (rename staging→final) pero
/// devuelve transitorio no debe fallar la task ni perder el `Created`: se
/// desambigua por presencia+tamaño del destino. Antes: el retry recopiaba,
/// su commit no-replace daba Conflict → task FALLIDA con el archivo bien
/// copiado y sin evento en el journal (regla 4).
#[tokio::test]
async fn commit_ambiguo_no_pierde_el_created() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///src.bin", b"contenido de prueba").await;
    // El PRÓXIMO commit aplica su efecto y devuelve transitorio (una vez).
    mem.faults().ambiguous_mutations(1);

    let h = engine
        .copy(&vp("mem:///src.bin"), &vp("mem:///dst.bin"))
        .await
        .expect("copy");
    assert_eq!(
        h.join().await,
        TaskState::Completed,
        "el commit aplicó: la task no debe fallar por el transitorio"
    );
    // El destino existe con el tamaño del origen.
    assert_eq!(
        mem.stat(&vp("mem:///dst.bin")).await.unwrap().size,
        Some(19)
    );
    // Y HAY exactamente un `Created` (regla 4): el undo lo conocerá.
    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es.len(), 1, "un único Created pese al commit ambiguo");
    assert_eq!(es[0].op, "created");
    assert_eq!(es[0].path, b"mem:///dst.bin");
}

/// #32.1 con archivo 0-byte: `final_size == 0` desambigua igual (el destino
/// existe con tamaño 0), un único `Created`.
#[tokio::test]
async fn commit_ambiguo_archivo_vacio() {
    let (engine, mem, journal) = setup().await;
    write_file(&mem, "mem:///vacio.bin", b"").await;
    mem.faults().ambiguous_mutations(1);

    let h = engine
        .copy(&vp("mem:///vacio.bin"), &vp("mem:///dst.bin"))
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(mem.stat(&vp("mem:///dst.bin")).await.unwrap().size, Some(0));
    let es = journal.journal().entries().await.expect("entries");
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].op, "created");
}

/// #32.2 — mkdir ambiguo: el mkdir del dir destino APLICA su efecto y
/// devuelve transitorio; el retry ve `Conflict`. Antes: bajo política Fail
/// la task FALLABA con el dir bien creado, y bajo merge completaba pero SIN
/// `Created` del dir (el undo de M3 no lo conocía). Ahora `ensure_dir`
/// pre-statea el destino: si NO preexistía, el Conflict ambiguo es nuestra
/// primera aplicación — la task completa y el journal registra el dir.
#[tokio::test]
async fn mkdir_ambiguo_no_pierde_el_created() {
    let (engine, mem, journal) = setup().await;
    mem.mkdir(&vp("mem:///d")).await.expect("mkdir src");
    write_file(&mem, "mem:///d/f.bin", b"contenido").await;
    // La PRÓXIMA mutación (el mkdir de mem:///d2) aplica y da transitorio.
    mem.faults().ambiguous_mutations(1);

    let h = engine
        .copy(&vp("mem:///d"), &vp("mem:///d2"))
        .await
        .expect("copy");
    assert_eq!(
        h.join().await,
        TaskState::Completed,
        "el mkdir aplicó: la task no debe fallar por el transitorio"
    );
    // El árbol copió entero.
    assert_eq!(
        mem.stat(&vp("mem:///d2/f.bin")).await.unwrap().size,
        Some(9)
    );
    // Y el journal tiene el `Created` del DIR (regla 4): el undo lo conoce.
    let es = journal.journal().entries().await.expect("entries");
    let dir_created = es
        .iter()
        .filter(|e| e.op == "created" && e.path == b"mem:///d2")
        .count();
    assert_eq!(dir_created, 1, "un único Created del dir ambiguo: {es:?}");
}

/// #32.2 (contracara): un dir destino PREEXISTENTE bajo merge sigue SIN
/// `Created` — el pre-stat sabe que no es nuestro y el undo jamás lo tocará.
#[tokio::test]
async fn mkdir_sobre_dir_preexistente_no_emite_created() {
    let (engine, mem, journal) = setup().await;
    mem.mkdir(&vp("mem:///d")).await.expect("mkdir src");
    write_file(&mem, "mem:///d/f.bin", b"contenido").await;
    mem.mkdir(&vp("mem:///d2")).await.expect("dst preexistente");

    let h = engine
        .copy_with(
            &vp("mem:///d"),
            &vp("mem:///d2"),
            norte_core::TransferOptions {
                on_collision: norte_proto::CollisionPolicy::Overwrite,
                ..Default::default()
            },
        )
        .await
        .expect("copy");
    assert_eq!(h.join().await, TaskState::Completed);
    let es = journal.journal().entries().await.expect("entries");
    assert!(
        !es.iter()
            .any(|e| e.op == "created" && e.path == b"mem:///d2"),
        "un dir preexistente jamás gana Created: {es:?}"
    );
}
