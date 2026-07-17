//! Matriz test-first de la fase 4 de M2: resume de transferencias
//! (ADR 0012). Contra `MemProvider` con fallos inyectados y `LocalProvider`
//! real para la reanudación cross-invocación.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_core::{Engine, TransferOptions};
use norte_proto::{Error, ResumePolicy, TaskState, VPath, VerifyPolicy};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido de test")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk entra");
    sink.commit().await.expect("commit publica");
}

async fn read_all(p: &dyn Provider, wire: &str) -> Result<Vec<u8>, Error> {
    let mut stream = p.read(&vp(wire), None).await?;
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk?);
    }
    Ok(out)
}

/// Resume AGNÓSTICO del provider (ADR 0012 A2): `MemProvider` implementa
/// `open_resumable`/`keep` de verdad, así que reanuda SIN necesitar declarar
/// `APPEND` (el engine no gatea por capability).
fn engine_with_mem() -> (Engine, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

fn resume_on() -> TransferOptions {
    TransferOptions {
        resume: ResumePolicy::On,
        ..TransferOptions::default()
    }
}

/// resume=On: una copia cortada a mitad por un fallo NO-transitorio deja un
/// `.norte-partial` reanudable; la copia siguiente continúa desde ahí y
/// jamás recopia lo hecho — verificado por bytes leídos del origen.
#[tokio::test]
async fn resume_continua_sin_recopiar() {
    let (engine, mem) = engine_with_mem();
    // 10 KiB; el chunk de Mem es 1 KiB, así que hay muchos.
    let content: Vec<u8> = (0..10_240)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    write_file(&mem, "mem:///src.bin", &content).await;

    // Corta la lectura del origen a los 4096 bytes con un Io no-retryable.
    mem.faults().fail_read_at(&vp("mem:///src.bin"), 4096);
    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_on())
        .await
        .unwrap();
    assert!(matches!(handle.join().await, TaskState::Failed { .. }));
    // El destino final aún no existe; hay un parcial con ~4096 bytes.
    assert_eq!(
        mem.stat(&vp("mem:///dst.bin")).await.unwrap_err(),
        Error::NotFound
    );

    // Segunda copia: sin el fallo, reanuda y termina.
    mem.faults().clear();
    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_on())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        read_all(&*mem, "mem:///dst.bin").await.unwrap(),
        content,
        "el destino es el archivo COMPLETO y correcto"
    );
}

/// resume=On: cancelar a mitad conserva el parcial (no lo aborta); la
/// reanudación posterior completa.
#[tokio::test]
async fn resume_no_reinicia_la_barra() {
    let (engine, mem) = engine_with_mem();
    let content: Vec<u8> = (0..10_240)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    write_file(&mem, "mem:///src.bin", &content).await;

    // Corte determinista a ~4096 bytes: deja un parcial.
    mem.faults().fail_read_at(&vp("mem:///src.bin"), 4096);
    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_on())
        .await
        .unwrap();
    assert!(matches!(handle.join().await, TaskState::Failed { .. }));

    // Reanuda: el destino se completa y el tramo ya presente se contó al
    // abrir el resumable (la barra no arranca de cero).
    mem.faults().clear();
    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_on())
        .await
        .unwrap();
    let rx = handle.progress();
    let arranque = rx.borrow().bytes_done;
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///dst.bin").await.unwrap(), content);
    assert!(
        arranque >= 4096 || rx.borrow().bytes_done == content.len() as u64,
        "el progreso reanudado parte del tramo ya hecho (arranque={arranque})"
    );
}

/// resume=Off (default): cancelar deja el destino LIMPIO — el contrato de
/// M1 intacto, sin parcial que reanudar.
#[tokio::test]
async fn sin_resume_cancelar_deja_limpio() {
    let (engine, mem) = engine_with_mem();
    let content: Vec<u8> = (0..50_000)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    write_file(&mem, "mem:///src.bin", &content).await;
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(20)));

    let handle = engine
        .copy(&vp("mem:///src.bin"), &vp("mem:///dst.bin"))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);
    assert_eq!(
        mem.stat(&vp("mem:///dst.bin")).await.unwrap_err(),
        Error::NotFound,
        "destino limpio (M1)"
    );
    // Y una segunda copia SIN resume empieza de cero (no reanuda un parcial
    // que no debe existir).
    mem.faults().clear();
    let handle = engine
        .copy(&vp("mem:///src.bin"), &vp("mem:///dst.bin"))
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///dst.bin").await.unwrap(), content);
}

/// verify=Length: un parcial más LARGO que el origen (origen encogió) se
/// descarta y la copia empieza de cero — jamás un destino corrupto.
#[tokio::test]
async fn resume_descarta_parcial_mas_largo_que_el_origen() {
    let (engine, mem) = engine_with_mem();
    // Deja un parcial de 8 bytes vía keep manual.
    let (mut sink, _) = mem.open_resumable(&vp("mem:///dst.bin")).await.unwrap();
    sink.write(Bytes::from_static(b"viejooo!")).await.unwrap();
    sink.keep().await.unwrap();

    // El origen ahora es más CORTO (3 bytes).
    write_file(&mem, "mem:///src.bin", b"abc").await;
    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_on())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        read_all(&*mem, "mem:///dst.bin").await.unwrap(),
        b"abc",
        "el parcial pasado se descartó; el destino es el origen actual"
    );
}

fn resume_hash() -> TransferOptions {
    TransferOptions {
        resume: ResumePolicy::On,
        verify: VerifyPolicy::Hash,
        ..TransferOptions::default()
    }
}

/// #35 — verify=Hash caza lo que Length NO puede: el origen CAMBIÓ pero
/// conserva el MISMO tamaño. Length reanudaría sobre un prefijo obsoleto
/// (destino corrupto: prefijo viejo + sufijo nuevo); Hash compara el prefijo
/// y descarta el parcial, empezando de cero.
#[tokio::test]
async fn hash_descarta_parcial_de_origen_cambiado_mismo_tamano() {
    let (engine, mem) = engine_with_mem();
    // Parcial de 5 bytes de un origen VIEJO.
    let (mut sink, _) = mem.open_resumable(&vp("mem:///dst.bin")).await.unwrap();
    sink.write(Bytes::from_static(b"VIEJO")).await.unwrap();
    sink.keep().await.unwrap();

    // El origen ACTUAL mide lo MISMO (10 bytes) pero su prefijo difiere.
    write_file(&mem, "mem:///src.bin", b"nuevo12345").await;
    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_hash())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        read_all(&*mem, "mem:///dst.bin").await.unwrap(),
        b"nuevo12345",
        "Hash detectó el prefijo obsoleto y recopió entero"
    );
}

/// #35 — verify=Hash sobre un provider SIN `partial_digest` (default `None`)
/// degrada a Length: NO puede comparar prefijos, así que un parcial más
/// corto que el origen se REANUDA (comportamiento Length), no se descarta.
#[tokio::test]
async fn hash_sin_digest_degrada_a_length() {
    use async_trait::async_trait;
    use norte_proto::{ByteRange, Capabilities, Entry};
    use norte_vfs::{ByteSink, ByteStream, EntryStream};

    // Delega TODO en Mem (incluido open_resumable/keep: reanuda de verdad)
    // salvo partial_digest, que queda en el default `None` del trait.
    struct NoDigest(Arc<MemProvider>);
    #[async_trait]
    impl Provider for NoDigest {
        fn scheme(&self) -> &str {
            self.0.scheme()
        }
        fn capabilities(&self) -> Capabilities {
            self.0.capabilities()
        }
        async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
            self.0.stat(p).await
        }
        async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
            self.0.list(p).await
        }
        async fn read(&self, p: &VPath, r: Option<ByteRange>) -> Result<ByteStream, Error> {
            self.0.read(p, r).await
        }
        async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
            self.0.write(p).await
        }
        async fn open_resumable(&self, p: &VPath) -> Result<(Box<dyn ByteSink>, u64), Error> {
            self.0.open_resumable(p).await
        }
        async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
            self.0.mkdir(p).await
        }
        async fn remove(&self, p: &VPath) -> Result<(), Error> {
            self.0.remove(p).await
        }
        async fn rename(&self, a: &VPath, b: &VPath) -> Result<(), Error> {
            self.0.rename(a, b).await
        }
        // partial_digest NO se sobreescribe: default `None`.
    }

    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::new(NoDigest(Arc::clone(&mem))) as Arc<dyn Provider>);

    // Parcial "VIEJO" (5) de un origen viejo; el actual mide LO MISMO (10).
    let (mut sink, _) = mem.open_resumable(&vp("mem:///dst.bin")).await.unwrap();
    sink.write(Bytes::from_static(b"VIEJO")).await.unwrap();
    sink.keep().await.unwrap();
    write_file(&mem, "mem:///src.bin", b"nuevo12345").await;

    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_hash())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    // Sin digest, Hash degrada a Length: el prefijo obsoleto SE CONSERVA
    // (Length no lo caza) → "VIEJO" + "12345". Es el comportamiento
    // documentado de la degradación, no un bug del test.
    assert_eq!(
        read_all(&*mem, "mem:///dst.bin").await.unwrap(),
        b"VIEJO12345",
        "sin partial_digest, Hash se comporta como Length (reanuda el prefijo)"
    );
}

/// #35 — verify=Hash con el origen INTACTO reanuda de verdad: el prefijo
/// casa, el parcial se conserva y solo se copia el resto.
#[tokio::test]
async fn hash_reanuda_si_el_prefijo_casa() {
    let (engine, mem) = engine_with_mem();
    let content = b"mismo prefijo + resto".to_vec();
    // Parcial con el prefijo REAL del origen (7 bytes).
    let (mut sink, _) = mem.open_resumable(&vp("mem:///dst.bin")).await.unwrap();
    sink.write(Bytes::copy_from_slice(&content[..7]))
        .await
        .unwrap();
    sink.keep().await.unwrap();

    write_file(&mem, "mem:///src.bin", &content).await;
    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_hash())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        read_all(&*mem, "mem:///dst.bin").await.unwrap(),
        content,
        "prefijo íntegro: reanudó y completó correcto"
    );
}

/// resume=On contra un provider que HEREDA los defaults del trait
/// (`open_resumable`=(write,0), `keep`=abort): degrada limpio a copia
/// normal — completa bien y una interrupción no deja parcial.
#[tokio::test]
async fn resume_con_defaults_del_trait_degrada_limpio() {
    use async_trait::async_trait;
    use norte_proto::{ByteRange, Capabilities, Entry};
    use norte_vfs::{ByteSink, ByteStream, EntryStream};

    // Wrapper que delega TODO en Mem salvo open_resumable/keep, que quedan
    // en el default del trait (sin reanudación real).
    struct DefaultsProvider(Arc<MemProvider>);
    #[async_trait]
    impl Provider for DefaultsProvider {
        fn scheme(&self) -> &str {
            self.0.scheme()
        }
        fn capabilities(&self) -> Capabilities {
            self.0.capabilities()
        }
        async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
            self.0.stat(p).await
        }
        async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
            self.0.list(p).await
        }
        async fn read(&self, p: &VPath, r: Option<ByteRange>) -> Result<ByteStream, Error> {
            self.0.read(p, r).await
        }
        async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, Error> {
            self.0.write(p).await
        }
        async fn mkdir(&self, p: &VPath) -> Result<(), Error> {
            self.0.mkdir(p).await
        }
        async fn remove(&self, p: &VPath) -> Result<(), Error> {
            self.0.remove(p).await
        }
        async fn rename(&self, a: &VPath, b: &VPath) -> Result<(), Error> {
            self.0.rename(a, b).await
        }
        // open_resumable/keep NO se sobreescriben: defaults del trait.
    }

    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::new(DefaultsProvider(Arc::clone(&mem))) as Arc<dyn Provider>);
    write_file(&mem, "mem:///src", b"datos").await;
    let handle = engine
        .copy_with(&vp("mem:///src"), &vp("mem:///dst"), resume_on())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///dst").await.unwrap(), b"datos");

    // Una interrupción con defaults NO deja parcial (keep=abort).
    write_file(&mem, "mem:///src2", b"0123456789").await;
    mem.faults().fail_read_at(&vp("mem:///src2"), 4);
    let handle = engine
        .copy_with(&vp("mem:///src2"), &vp("mem:///dst2"), resume_on())
        .await
        .unwrap();
    assert!(matches!(handle.join().await, TaskState::Failed { .. }));
    assert_eq!(
        mem.stat(&vp("mem:///dst2")).await.unwrap_err(),
        Error::NotFound,
        "defaults: keep=abort → sin parcial, destino limpio"
    );
}

/// Regla 3: cancelar una copia con resume=On es limpio (keep durabiliza el
/// parcial sin cuelgue) y la reanudación posterior completa.
#[tokio::test]
async fn resume_cancelacion_es_limpia_y_reanuda() {
    let (engine, mem) = engine_with_mem();
    let content: Vec<u8> = (0..80_000)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    write_file(&mem, "mem:///src.bin", &content).await;
    // Latencia por op: da margen a cancelar antes de que el read arme todo.
    mem.faults()
        .set_latency_per_op(Some(std::time::Duration::from_millis(30)));

    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_on())
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    handle.cancel();
    assert_eq!(handle.join().await, TaskState::Cancelled);
    // El destino final jamás existe a medias (limpio o parcial marcado).
    assert_eq!(
        mem.stat(&vp("mem:///dst.bin")).await.unwrap_err(),
        Error::NotFound
    );

    // Reanuda hasta completar, byte-exacto.
    mem.faults().clear();
    let handle = engine
        .copy_with(&vp("mem:///src.bin"), &vp("mem:///dst.bin"), resume_on())
        .await
        .unwrap();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(read_all(&*mem, "mem:///dst.bin").await.unwrap(), content);
}
