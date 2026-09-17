//! Integración `Engine::dir_usage_as` (fase 4): de qué está HECHO un
//! directorio, como Task cancelable, con los hijos en un INFORME — porque una
//! lista no cabe en el desenlace de una Task ni en su progreso.
//!
//! Es el hermano de `engine_dir_size`, con la pregunta al revés: aquello
//! contesta «¿cuánto ocupa esto?» en un número, y por eso le bastaba el
//! progreso. Un mapa necesita la lista, y de ahí el informe.
//!
//! `MemProvider` in-memory → determinista, sin tocar disco.

use std::sync::Arc;

use bytes::Bytes;
use norte_core::{Actor, Engine};
use norte_proto::methods::FsDirUsageParams;
use norte_proto::{EntryKind, Error as ProtoError, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

async fn write_file(mem: &MemProvider, wire: &str, bytes: usize) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::from(vec![b'x'; bytes]))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

async fn mkdir(mem: &MemProvider, wire: &str) {
    mem.mkdir(&vp(wire)).await.expect("mkdir");
}

fn setup() -> (Engine, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

fn params(wire: &str) -> FsDirUsageParams {
    FsDirUsageParams {
        path: vp(wire),
        depth: 1,
    }
}

/// Busca un hijo por su nombre en bytes, que es como se nombran (regla 1).
fn hijo<'a>(
    informe: &'a norte_proto::methods::FsDirUsageReportResult,
    name: &[u8],
) -> &'a norte_proto::methods::DirUsageChild {
    informe
        .children
        .iter()
        .find(|c| c.name.as_bytes() == name)
        .unwrap_or_else(|| panic!("no está el hijo {}", String::from_utf8_lossy(name)))
}

/// Lo que la feature promete: cada hijo con lo que ocupa ENTERO, subárbol
/// incluido. Es la diferencia con un listado —que da el tamaño del nodo— y lo
/// único con lo que se puede pintar un mapa.
#[tokio::test]
async fn el_mapa_dice_de_que_esta_hecho_el_directorio() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///raiz").await;
    mkdir(&mem, "mem:///raiz/sub").await;
    write_file(&mem, "mem:///raiz/suelto", 10).await;
    write_file(&mem, "mem:///raiz/sub/b", 32).await;
    write_file(&mem, "mem:///raiz/sub/c", 8).await;

    let handle = engine
        .dir_usage_as(params("mem:///raiz"), Actor::User)
        .await
        .expect("lanza");
    let id = handle.id();
    assert_eq!(handle.join().await, TaskState::Completed);

    let (_actor, informe) = engine.dir_usage_report(id).expect("hay mapa");
    assert!(informe.listed, "el listado de la raíz terminó");
    assert_eq!(informe.pending, 0, "terminada: no queda hijo por medir");
    assert_eq!(informe.omitted, 0);
    assert_eq!(informe.children.len(), 2, "un suelto y un subdirectorio");

    let suelto = hijo(&informe, b"suelto");
    assert_eq!(suelto.kind, EntryKind::File);
    assert_eq!(suelto.bytes, 10);
    assert_eq!(suelto.entries, 1);
    assert!(!suelto.partial);

    // Lo que hace que esto sea un MAPA: el directorio pesa lo que pesa su
    // contenido, no cero.
    let sub = hijo(&informe, b"sub");
    assert_eq!(sub.kind, EntryKind::Dir);
    assert_eq!(sub.bytes, 40, "32 + 8, el subárbol entero");
    assert_eq!(sub.entries, 3, "él y sus dos ficheros");
    assert!(!sub.partial);

    assert_eq!(informe.total_bytes, 50);
    assert_eq!(informe.total_entries, 4);
}

/// El nombre de un hijo son sus BYTES (regla 1), no un `String` lossy: un
/// nombre que no es UTF-8 existe en el disco y el mapa tiene que poder
/// nombrarlo.
#[tokio::test]
async fn el_nombre_de_un_hijo_son_los_bytes_que_habia_en_el_disco() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///raiz").await;
    write_file(&mem, "mem:///raiz/caf%FF.txt", 3).await;

    let handle = engine
        .dir_usage_as(params("mem:///raiz"), Actor::User)
        .await
        .expect("lanza");
    let id = handle.id();
    assert_eq!(handle.join().await, TaskState::Completed);

    let (_actor, informe) = engine.dir_usage_report(id).expect("mapa");
    assert_eq!(informe.children.len(), 1);
    assert_eq!(
        informe.children[0].name.as_bytes(),
        b"caf\xFF.txt",
        "los bytes vuelven tal cual, sin pasar por un lossy"
    );
}

/// Un hijo que no se deja medir del todo sale MARCADO, y los demás se miden.
///
/// `partial` va por hijo y no por informe, que es la diferencia entre poder
/// pintar el mapa y no: se marca el rectángulo incompleto y el resto sigue
/// siendo verdad. Una bandera global solo puede apagar el mapa entero.
#[tokio::test]
async fn un_hijo_ilegible_sale_marcado_y_no_tumba_el_mapa() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///raiz").await;
    mkdir(&mem, "mem:///raiz/prohibido").await;
    mkdir(&mem, "mem:///raiz/abierto").await;
    write_file(&mem, "mem:///raiz/prohibido/secreto", 1000).await;
    write_file(&mem, "mem:///raiz/abierto/x", 7).await;
    mem.faults().fail_list_at(&vp("mem:///raiz/prohibido"));

    let handle = engine
        .dir_usage_as(params("mem:///raiz"), Actor::User)
        .await
        .expect("lanza");
    let id = handle.id();
    assert_eq!(
        handle.join().await,
        TaskState::Completed,
        "un hijo ilegible NO hace fallar la Task"
    );

    let (_actor, informe) = engine.dir_usage_report(id).expect("mapa");
    let prohibido = hijo(&informe, b"prohibido");
    assert!(
        prohibido.partial,
        "lo suyo es una cota inferior, y se DECLARA"
    );
    assert_eq!(prohibido.bytes, 0, "los 1000 no se pudieron leer");

    let abierto = hijo(&informe, b"abierto");
    assert!(!abierto.partial, "el hermano sano no se contagia");
    assert_eq!(abierto.bytes, 7);
}

/// De qué está hecho un FICHERO no es una pregunta: está hecho de sí mismo.
///
/// Se rechaza en vez de contestar con un mapa de un solo rectángulo, que es la
/// respuesta que parece útil y no lo es.
#[tokio::test]
async fn describir_un_fichero_no_es_una_pregunta() {
    let (engine, mem) = setup();
    write_file(&mem, "mem:///solo", 7).await;

    let handle = engine
        .dir_usage_as(params("mem:///solo"), Actor::User)
        .await
        .expect("lanza");
    // Y falla POR LO QUE ES. Un `Failed` a secas también lo daría un panic
    // capturado (`Error::Internal` con `panic: true`), así que sin mirar la
    // causa este test pasaría el día que medir un fichero reventase.
    let TaskState::Failed { error } = handle.join().await else {
        panic!("de qué está hecho un fichero no es una pregunta");
    };
    assert!(matches!(error, ProtoError::InvalidPath), "{error:?}");
}

/// La profundidad se comprueba ANTES de crear Task alguna, y las de más se
/// RECHAZAN en vez de recortarse.
///
/// Recortar en silencio deja al cliente creyendo que tiene los dos niveles que
/// pidió: pintaría un mapa de un nivel diciendo que es de dos. Por eso `2` es
/// `Unsupported` —«el método existe, esa profundidad no se sirve»— y no un
/// `1` disfrazado.
#[tokio::test]
async fn una_profundidad_que_no_se_sirve_se_rechaza_y_no_se_recorta() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///raiz").await;

    let cero = FsDirUsageParams {
        path: vp("mem:///raiz"),
        depth: 0,
    };
    let Err(err) = engine.dir_usage_as(cero, Actor::User).await else {
        panic!("describir cero niveles no es una petición");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "{err:?}");

    let pasada = FsDirUsageParams {
        path: vp("mem:///raiz"),
        depth: norte_proto::methods::DIR_USAGE_MAX_DEPTH + 1,
    };
    let Err(err) = engine.dir_usage_as(pasada, Actor::User).await else {
        panic!("por encima del tope tiene que rechazarse");
    };
    assert!(matches!(err, ProtoError::InvalidPath), "{err:?}");

    let dos = FsDirUsageParams {
        path: vp("mem:///raiz"),
        depth: 2,
    };
    let Err(err) = engine.dir_usage_as(dos, Actor::User).await else {
        panic!("hoy solo se sirve un nivel, y se dice");
    };
    assert!(
        matches!(err, ProtoError::Unsupported),
        "«no se sirve», que no es «no vale»: {err:?}"
    );
}

/// Regla 3: el mapa se cancela limpiamente ESTANDO DENTRO, y el informe se
/// queda a medias diciéndolo.
///
/// La primera versión de este test cancelaba justo después de lanzar, y no
/// probaba nada: el cuerpo corre en otro spawn, así que la cancelación llegaba
/// antes de la primera entrada, el informe se quedaba en `default()` y los dos
/// asserts se cumplían solos. Habría pasado igual con TODAS las comprobaciones
/// de cancelación borradas — que es la definición de un test verde que no
/// demuestra nada.
///
/// Así que la cancelación se arma DONDE pasa el tiempo: al tercer `list`, o
/// sea con la raíz ya listada y un hijo ya medido, mientras se mide el
/// siguiente. Determinista y sin reloj — un `sleep` acertaría por casualidad.
///
/// El estado que se fija solo lo puede producir un corte a media medición: hay
/// un hijo medido Y el listado no llegó a terminar. Eso es lo que `listed`
/// existe para decir: sin él, un mapa con un hijo se lee igual que un
/// directorio que solo tiene uno.
#[tokio::test]
async fn cancelar_a_media_medicion_deja_el_mapa_marcado_como_incompleto() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///grande").await;
    mkdir(&mem, "mem:///grande/a").await;
    mkdir(&mem, "mem:///grande/b").await;
    write_file(&mem, "mem:///grande/a/x", 10).await;
    write_file(&mem, "mem:///grande/b/y", 20).await;

    let handle = engine
        .dir_usage_as(params("mem:///grande"), Actor::User)
        .await
        .expect("lanza");
    // 1 = la raíz, 2 = el subárbol de `a`, 3 = el de `b`: corta en el tercero.
    mem.faults().cancel_after_lists(3, handle.cancel_token());
    let id = handle.id();
    assert_eq!(handle.join().await, TaskState::Cancelled);

    let (_actor, informe) = engine.dir_usage_report(id).expect("hay mapa");
    assert!(
        !informe.listed,
        "el listado NO terminó, y el informe no puede insinuar que sí"
    );
    assert_eq!(
        informe.children.len(),
        1,
        "se midió `a` y se cortó en `b`: si fueran 0 la cancelación llegó \
         antes de empezar y este test no prueba que el bucle interior pare"
    );
}

/// Por encima del tope sobreviven los MÁS GRANDES, y lo omitido se cuenta en
/// los totales aunque pierda su nombre.
///
/// El hijo enorme se crea el ÚLTIMO a propósito. Con hijos todos del mismo
/// tamaño este test pasaba con cualquier política —los primeros N y los mayores
/// N son el mismo conjunto—, que es justo cómo se me coló quedarme con los
/// primeros: el protocolo promete los mayores, y un mapa que manda el hijo de
/// 400 GB a `omitted` para pintar 4096 minucias es la función no existiendo.
///
/// Lo que el tope se lleva es el NOMBRE, no el tamaño: por eso los totales
/// siguen cuadrando y un mapa puede pintar el resto como un rectángulo más.
#[tokio::test]
async fn por_encima_del_tope_sobreviven_los_mas_grandes() {
    let (engine, mem) = setup();
    let tope = norte_proto::methods::DIR_USAGE_MAX_CHILDREN;
    mkdir(&mem, "mem:///muchos").await;
    for i in 0..tope {
        write_file(&mem, &format!("mem:///muchos/f{i}"), 2).await;
    }
    // El último en llegar y el mayor de todos.
    write_file(&mem, "mem:///muchos/enorme", 100_000).await;

    let handle = engine
        .dir_usage_as(params("mem:///muchos"), Actor::User)
        .await
        .expect("lanza");
    let id = handle.id();
    assert_eq!(handle.join().await, TaskState::Completed);

    let (_actor, informe) = engine.dir_usage_report(id).expect("mapa");
    assert_eq!(informe.children.len(), tope, "la lista se para en el tope");
    assert_eq!(informe.omitted, 1, "y dice cuántos se quedaron fuera");
    assert!(
        informe
            .children
            .iter()
            .any(|c| c.name.as_bytes() == b"enorme"),
        "el mayor viaja aunque llegue el último: es lo que un mapa pinta"
    );
    assert_eq!(
        informe.total_bytes,
        (tope as u64) * 2 + 100_000,
        "los totales los cuentan TODOS, también al que no se nombra"
    );
    assert_eq!(informe.total_entries, tope as u64 + 1);
}

/// Un provider que ADMITE haber dejado entradas fuera no produce un mapa que
/// diga «esto es todo».
///
/// El índice de un archivo deja fuera lo que no puede representar y lo cuenta
/// en `list_skipped` (#93). Sin mirarlo, el informe salía `listed: true`,
/// `omitted: 0` y todos los hijos `partial: false` — las tres señales de
/// completitud a la vez, sobre un listado que el propio provider dijo que
/// estaba incompleto.
///
/// Va a `unvisited` y no a `omitted` porque `omitted` promete que los bytes de
/// lo omitido SÍ están en los totales, y los de una entrada que nadie listó no
/// lo están.
#[tokio::test]
async fn un_listado_que_el_provider_recorto_no_se_anuncia_como_completo() {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new().with_list_skipped(3));
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    mkdir(&mem, "mem:///archivo").await;
    write_file(&mem, "mem:///archivo/visible", 5).await;

    let handle = engine
        .dir_usage_as(params("mem:///archivo"), Actor::User)
        .await
        .expect("lanza");
    let prog = handle.progress();
    assert_eq!(handle.join().await, TaskState::Completed);
    assert_eq!(
        prog.borrow().unvisited,
        Some(3),
        "el árbol es más grande que lo que se recorrió, y se dice"
    );
}

/// Dos hijos con bytes DISTINTOS que colapsan al mismo nombre lossy siguen
/// siendo dos hijos, con sus bytes intactos.
///
/// `\xFF.rs` y `\xFE.rs` se convierten los dos en `�.rs` en cuanto alguien hace
/// una conversión lossy. Con un solo nombre hostil el test no distingue «pasa
/// los bytes» de «los pliega»: hacen falta los DOS, que es justo por qué el
/// corpus los trae emparejados. Si alguien mete un `to_string_lossy` en este
/// camino, el mapa pinta un rectángulo donde había dos y `nav.enter` abre el
/// que no es.
#[tokio::test]
async fn dos_hijos_que_colapsan_al_mismo_lossy_siguen_siendo_dos() {
    let (engine, mem) = setup();
    let raiz = vp("mem:///hostil");
    mem.mkdir(&raiz).await.expect("mkdir");

    let fixture = |id: &str| {
        norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("fixture {id} en el corpus"))
            .bytes
    };
    let ff = fixture("lossy_collapse_ff");
    let fe = fixture("lossy_collapse_fe");
    for bytes in [ff.clone(), fe.clone()] {
        let seg = norte_proto::Segment::new(bytes).expect("segmento válido");
        let mut sink = mem.write(&raiz.join(seg)).await.expect("write abre");
        sink.write(Bytes::from_static(b"xy")).await.expect("chunk");
        sink.commit().await.expect("commit");
    }

    let handle = engine
        .dir_usage_as(params("mem:///hostil"), Actor::User)
        .await
        .expect("lanza");
    let id = handle.id();
    assert_eq!(handle.join().await, TaskState::Completed);

    let (_actor, informe) = engine.dir_usage_report(id).expect("mapa");
    assert_eq!(
        informe.children.len(),
        2,
        "dos nombres distintos, dos hijos: un pliegue lossy los haría uno"
    );
    assert!(
        informe.children.iter().any(|c| c.name.as_bytes() == ff),
        "los bytes de 0xFF vuelven tal cual"
    );
    assert!(
        informe.children.iter().any(|c| c.name.as_bytes() == fe),
        "y los de 0xFE también, distintos de los otros"
    );
}

/// Un id que nunca fue un mapa no tiene informe — y eso es lo que el daemon
/// convierte en `NotFound` para quien pregunta por el de otro.
#[tokio::test]
async fn un_id_ajeno_no_tiene_mapa() {
    let (engine, _mem) = setup();
    assert!(
        engine
            .dir_usage_report(norte_proto::TaskId::new(4242))
            .is_none()
    );
}
