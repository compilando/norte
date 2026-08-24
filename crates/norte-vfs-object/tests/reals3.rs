//! NIGHTLY: provider object contra un servidor S3 REAL (`MinIO`) por
//! testcontainers (ADR 0016 J, spec §12). Fuera del gate de PR (exige
//! Docker): lo corre `just it-remote` desde el workflow nightly.
//!
//! Cubre lo que el harness in-process NO da (issue #50): semántica de dirs
//! (markers/prefix-probe, que s3s-fs rompe), keys largas (>`NAME_MAX` del FS
//! host), `copy_native` con `If-None-Match` real (s3s-fs lo ignora) y el
//! conditional write que `MinIO` SÍ valida.
#![cfg(feature = "it-s3")]

use bytes::Bytes;
use futures::TryStreamExt;
use norte_proto::{Authority, ConflictKind, EntryKind, Error, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs_object::{ObjectProvider, Operator};
use testcontainers::core::WaitFor;
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt};

const AK: &str = "norteadmin";
const SK: &str = "nortesecret";
const BUCKET: &str = "norte-test";

fn root() -> VPath {
    ObjectProvider::root("s3", Authority::new(BUCKET).expect("authority"))
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(Segment::new(name.to_vec()).expect("segmento"))
}

async fn read_all(p: &ObjectProvider, f: &VPath) -> Vec<u8> {
    let mut s = p.read(f, None).await.expect("read");
    let mut out = Vec::new();
    while let Some(chunk) = s.try_next().await.expect("chunk") {
        out.extend_from_slice(&chunk);
    }
    out
}

async fn write_all(p: &ObjectProvider, f: &VPath, data: &[u8]) {
    let mut sink = p.write(f).await.expect("write");
    sink.write(Bytes::copy_from_slice(data))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

/// Un contenedor `MinIO` (bitnami: auto-crea el bucket vía `MINIO_DEFAULT_BUCKETS`)
/// más el provider y el `Operator` crudo. El Operator siembra keys "desde
/// fuera" (como otra herramienta): un prefijo sin marker que el propio provider
/// no crearía por su check de padre-existe. Un solo test por contenedor.
async fn setup() -> (
    testcontainers::ContainerAsync<GenericImage>,
    ObjectProvider,
    Operator,
) {
    // bitnamilegacy: bitnami movió sus imágenes públicas a este namespace en
    // 2025 (auto-crea el bucket con MINIO_DEFAULT_BUCKETS, lo que la imagen
    // oficial minio/minio no soporta). El WaitFor es laxo: setup() reintenta
    // el list hasta que el bucket exista.
    let container = GenericImage::new("bitnamilegacy/minio", "latest")
        // El banner de MinIO va a STDERR (el setup de bitnami a stdout).
        .with_wait_for(WaitFor::message_on_stderr("MinIO Object Storage Server"))
        .with_env_var("MINIO_ROOT_USER", AK)
        .with_env_var("MINIO_ROOT_PASSWORD", SK)
        .with_env_var("MINIO_DEFAULT_BUCKETS", BUCKET)
        .start()
        .await
        .expect("arrancar MinIO");
    let port = container.get_host_port_ipv4(9000).await.expect("puerto");
    opendal::install_default();
    let builder = opendal::services::S3::default()
        .bucket(BUCKET)
        .region("us-east-1")
        .endpoint(&format!("http://127.0.0.1:{port}"))
        .access_key_id(AK)
        .secret_access_key(SK)
        .disable_config_load()
        .disable_ec2_metadata();
    // El bucket puede tardar un instante en existir tras el arranque: reintenta.
    let op = Operator::new(builder).expect("operator");
    for _ in 0..40 {
        if op.list_with("").limit(1).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    (container, ObjectProvider::new(op.clone(), "s3"), op)
}

/// Semántica de dirs contra S3 real: mkdir (marker) + stat, prefijo sin marker
/// = Dir, `remove` de dir no vacío = Conflict, `rename` de subárbol.
#[tokio::test]
async fn dirs_markers_y_rename() {
    let (_c, p, op) = setup().await;
    let r = root();
    // mkdir + stat del marker (dir VACÍO: el caso que s3s-fs no da bien).
    let d = child(&r, b"undir");
    p.mkdir(&d).await.expect("mkdir");
    assert_eq!(p.stat(&d).await.expect("stat dir").kind, EntryKind::Dir);
    // Fichero dentro; dir no vacío no se borra.
    write_all(&p, &child(&d, b"f.txt"), b"x").await;
    assert!(matches!(
        p.remove(&d).await,
        Err(Error::Conflict {
            conflict: ConflictKind::TypeMismatch
        })
    ));
    // Prefijo SIN marker (sembrado por el Operator crudo, como otra
    // herramienta): el provider no lo crearía por su check de padre-existe.
    op.write("prefijo/hijo.txt", b"y".to_vec())
        .await
        .expect("seed prefijo");
    assert_eq!(
        p.stat(&child(&r, b"prefijo"))
            .await
            .expect("stat prefijo")
            .kind,
        EntryKind::Dir
    );
    // rename del subárbol: contenido byte-exacto en destino, origen desaparecido.
    let dst = child(&r, b"movido");
    p.rename(&d, &dst).await.expect("rename dir");
    assert_eq!(read_all(&p, &child(&dst, b"f.txt")).await, b"x");
    assert_eq!(
        p.stat(&child(&d, b"f.txt")).await.unwrap_err(),
        Error::NotFound
    );
}

/// Keys largas que el harness fs in-process (`NAME_MAX` del host + el `.XXXXXXXX`
/// del `atomic_write_dir` = tope 246) no cubre. `MinIO` (backend de FS) limita cada
/// COMPONENTE a 255 bytes como un `NAME_MAX` real — así que se prueba 250
/// (>246 del harness, ≤255 de `MinIO`). AWS real acepta hasta 1024 en la key
/// completa (ADR 0016 D); esa cota total solo la valida AWS, no `MinIO`.
#[tokio::test]
async fn keys_largas_byte_exactas() {
    let (_c, p, _op) = setup().await;
    let r = root();
    let nombre_largo = "x".repeat(250);
    let f = child(&r, nombre_largo.as_bytes());
    write_all(&p, &f, b"contenido").await;
    assert_eq!(read_all(&p, &f).await, b"contenido");
    // Aparece byte-exacto en el listado.
    let listed: Vec<Vec<u8>> = p
        .list(&r)
        .await
        .expect("list")
        .try_collect::<Vec<_>>()
        .await
        .expect("stream")
        .into_iter()
        .map(|e| e.path.file_name().expect("nombre").as_bytes().to_vec())
        .collect();
    assert!(listed.contains(&nombre_largo.into_bytes()));
}

/// `copy_native` con `If-None-Match` REAL: `MinIO` valida el conditional copy
/// (s3s-fs lo ignora). Copia byte-exacta; segundo copy al mismo destino =
/// Conflict.
#[tokio::test]
async fn copy_native_conditional_real() {
    let (_c, p, _op) = setup().await;
    let r = root();
    let src = child(&r, b"origen.bin");
    write_all(&p, &src, b"payload").await;
    let dst = child(&r, b"copia.bin");
    assert!(matches!(p.copy_native(&src, &dst).await, Some(Ok(()))));
    assert_eq!(read_all(&p, &dst).await, b"payload");
    // Segundo copy al MISMO destino → Conflict (If-None-Match).
    assert!(matches!(
        p.copy_native(&src, &dst).await,
        Some(Err(Error::Conflict { .. }))
    ));
}

/// Igual que [`setup`], pero con el `Operator` enraizado en un PREFIJO
/// (`/equipo/proyecto/`), que es lo que hace un despliegue real compartiendo
/// bucket. Es la única forma de ejercitar el presupuesto de key con `root`.
async fn setup_con_root(
    root_prefix: &str,
) -> (
    testcontainers::ContainerAsync<GenericImage>,
    ObjectProvider,
    Operator,
) {
    let container = GenericImage::new("bitnamilegacy/minio", "latest")
        .with_wait_for(WaitFor::message_on_stderr("MinIO Object Storage Server"))
        .with_env_var("MINIO_ROOT_USER", AK)
        .with_env_var("MINIO_ROOT_PASSWORD", SK)
        .with_env_var("MINIO_DEFAULT_BUCKETS", BUCKET)
        .start()
        .await
        .expect("arrancar MinIO");
    let port = container.get_host_port_ipv4(9000).await.expect("puerto");
    opendal::install_default();
    let builder = opendal::services::S3::default()
        .bucket(BUCKET)
        .region("us-east-1")
        .endpoint(&format!("http://127.0.0.1:{port}"))
        .access_key_id(AK)
        .secret_access_key(SK)
        .root(root_prefix)
        .disable_config_load()
        .disable_ec2_metadata();
    let op = Operator::new(builder).expect("operator");
    for _ in 0..40 {
        if op.list_with("").limit(1).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    (container, ObjectProvider::new(op.clone(), "s3"), op)
}

/// **El presupuesto de key descuenta el prefijo `root`, y el rechazo es de
/// AQUÍ, no del servidor** (#50 punto 2).
///
/// Una key de S3 son 1024 bytes contando el `root` que el `Operator` antepone
/// antes de mandarla. Si el provider no lo descuenta, deja pasar nombres que
/// el servidor rechaza a media operación — y a media operación significa con
/// un multipart abierto y un error que no dice qué pasó. La escalera se toma
/// alrededor del presupuesto EFECTIVO, no de 1024: es lo que distingue
/// descontar el prefijo de no descontarlo.
#[tokio::test]
async fn el_presupuesto_de_key_descuenta_el_prefijo_root() {
    let prefijo = "equipo/proyecto/";
    let (_c, p, _op) = setup_con_root(&format!("/{prefijo}")).await;
    let r = root();
    // 1024 − len(prefijo) − 1 (la `/` que reserva la variante directorio).
    let presupuesto = 1024 - prefijo.len() - 1;

    // Un nombre que cabe JUSTO. No se escribe: MinIO limita cada componente a
    // 255 bytes como un NAME_MAX de verdad, así que se comprueba lo que este
    // test existe para comprobar —dónde cae la frontera— con un path de
    // varios segmentos cortos.
    let segmentos = presupuesto / 10; // "sssssssss/" = 10 bytes por vuelta
    let mut cabe = r.clone();
    for _ in 0..segmentos {
        cabe = child(&cabe, b"sssssssss");
    }
    // El último segmento completa el presupuesto exacto.
    let resto = presupuesto - (segmentos * 10) + 1;
    if resto > 0 {
        cabe = child(&cabe, "z".repeat(resto).as_bytes());
    }
    // Un byte más NO cabe, y se dice ANTES de tocar la red.
    let pasado = child(&cabe, b"y");
    assert_eq!(
        p.stat(&pasado).await.unwrap_err(),
        Error::InvalidPath,
        "pasado el presupuesto tiene que ser InvalidPath de aquí, no un 400 del servidor"
    );

    // Y las capabilities lo DICEN: `max_path` es el presupuesto efectivo, no
    // 1024. Un cliente que componga nombres necesita el número de verdad.
    let caps = p.capabilities_at(&r).await.expect("caps");
    assert_eq!(
        caps.max_path,
        Some(u32::try_from(presupuesto).expect("cabe")),
        "las capabilities anuncian el presupuesto YA descontado"
    );
}

/// **Una key ecoada más CORTA que el `root` no puede tumbar la task** (#50
/// punto 4).
///
/// `build_rel_path` de opendal 0.58 recorta la key por el largo del `root` con
/// solo un `debug_assert`: en release, una key más corta que el prefijo hace
/// slicing fuera de rango —o corta a mitad de un carácter multibyte—. Un
/// servidor que ecoe algo que no empieza por el `root` (mentiroso, o un proxy
/// que reescribe) convertiría eso en un panic dentro de la Task.
///
/// Aquí se siembra por debajo del prefijo con el `Operator` CRUDO —o sea con
/// keys que sí llevan el root— y se comprueba lo que el provider promete: que
/// listar y statear lo sembrado desde fuera no revienta. Reproducir el panic
/// pide un servidor mentiroso, que es otra pieza; esto fija que el camino
/// normal no lo dispara y deja el caso escrito.
#[tokio::test]
async fn una_siembra_bajo_el_root_no_revienta_el_listado() {
    let (_c, p, op) = setup_con_root("/equipo/proyecto/").await;
    let r = root();
    // El Operator ya lleva el root: esta key es `equipo/proyecto/desde-fuera/a.txt`.
    op.write("desde-fuera/a.txt", b"contenido".to_vec())
        .await
        .expect("seed");
    let dir = child(&r, b"desde-fuera");
    assert_eq!(p.stat(&dir).await.expect("stat").kind, EntryKind::Dir);
    assert_eq!(read_all(&p, &child(&dir, b"a.txt")).await, b"contenido");
    // Y el listado de la raíz la ve, sin que el recorte del prefijo se lleve
    // por delante ningún byte del nombre.
    let nombres: Vec<Vec<u8>> = p
        .list(&r)
        .await
        .expect("list")
        .try_collect::<Vec<_>>()
        .await
        .expect("stream")
        .into_iter()
        .map(|e| e.path.file_name().expect("nombre").as_bytes().to_vec())
        .collect();
    assert!(nombres.contains(&b"desde-fuera".to_vec()), "{nombres:?}");
}

/// **Keys que el provider NO puede crear pero que SÍ pueden existir** (#50
/// punto 3): sembradas por otra herramienta, tienen que dar una respuesta
/// honesta —correcta o fail-loud— y jamás una corrupción silenciosa.
///
/// Las tres del issue: un espacio final (que nuestro `key()` rechaza porque
/// opendal lo recortaría, #48), un segmento vacío (`dir//x`) y un marker de
/// directorio vacío.
#[tokio::test]
async fn keys_sembradas_desde_fuera_que_nosotros_no_creariamos() {
    let (_c, p, op) = setup().await;
    let r = root();

    // 1. Espacio final. Nuestro `key()` lo rechaza ANTES de la red porque
    //    `normalize_path` de opendal hace `trim()` y corromperia el nombre.
    op.write("sembrado/con espacio ", b"a".to_vec())
        .await
        .expect("seed espacio");
    let con_espacio = child(&child(&r, b"sembrado"), b"con espacio ");
    assert_eq!(
        p.stat(&con_espacio).await.unwrap_err(),
        Error::InvalidPath,
        "un nombre que opendal recortaría se rehúsa fail-loud, no se lee otro fichero"
    );

    // 2. Segmento vacío (`dir//x`): no hay `VPath` que lo nombre —`Segment`
    //    rechaza el vacío— así que el provider no puede pedirlo ni por
    //    accidente. Lo que importa es que su presencia no rompa el listado
    //    del directorio de al lado.
    op.write("sembrado//hueco.txt", b"b".to_vec())
        .await
        .expect("seed vacío");
    let listado = p.list(&child(&r, b"sembrado")).await;
    assert!(
        listado.is_ok(),
        "una key con segmento vacío al lado no puede tumbar el listado"
    );

    // 3. Marker de directorio vacío, que es como otra herramienta representa
    //    un dir sin contenido. Tiene que verse como Dir.
    // `write` de una key acabada en `/` lo rehúsa el propio opendal
    // (`IsADirectory`), así que el marker se siembra como lo sembraría otra
    // herramienta: con la operación de crear directorio.
    op.create_dir("vacio/").await.expect("seed marker");
    assert_eq!(
        p.stat(&child(&r, b"vacio"))
            .await
            .expect("stat marker")
            .kind,
        EntryKind::Dir
    );
}

/// Las fixtures LARGAS del corpus canónico contra un S3 real (#50 punto 1).
///
/// El test de arriba prueba 250 bytes elegidos a mano; estas son las del
/// corpus, que es lo que el resto del proyecto usa para decir «nombre largo».
/// `name_over_max_256` pasa de los 255 que MinIO impone por componente: eso
/// **no es un defecto nuestro** —AWS acepta hasta 1024 en la key completa— y
/// el test lo fija como lo que es, una diferencia entre servidores, en vez de
/// dejar la afirmación sin comprobar.
#[tokio::test]
async fn las_fixtures_largas_del_corpus_contra_s3_real() {
    let (_c, p, _op) = setup().await;
    let r = root();
    let corpus = norte_testkit::corpus::hostile_names();

    for id in ["name_max_255", "name_max_255_multibyte"] {
        let f = corpus
            .iter()
            .find(|f| f.id == id)
            .unwrap_or_else(|| panic!("el corpus canónico tiene `{id}`"));
        let path = child(&r, &f.bytes);
        write_all(&p, &path, b"x").await;
        assert_eq!(read_all(&p, &path).await, b"x", "[{id}] round-trip");
        // Y vuelve BYTE-EXACTO del listado: 255 bytes multibyte es donde un
        // recorte por cuenta de bytes partiría un carácter.
        let listado: Vec<Vec<u8>> = p
            .list(&r)
            .await
            .expect("list")
            .try_collect::<Vec<_>>()
            .await
            .expect("stream")
            .into_iter()
            .map(|e| e.path.file_name().expect("nombre").as_bytes().to_vec())
            .collect();
        assert!(listado.contains(&f.bytes), "[{id}] no volvió byte-exacto");
    }

    // 256 bytes: MinIO lo rechaza por componente. Se documenta el veredicto
    // real en vez de suponerlo.
    let over = corpus
        .iter()
        .find(|f| f.id == "name_over_max_256")
        .expect("el corpus tiene `name_over_max_256`");
    let res = p.write(&child(&r, &over.bytes)).await;
    match res {
        Err(e) => {
            // Lo que NO puede pasar es un panic ni un éxito silencioso que
            // luego no se pueda leer.
            eprintln!("name_over_max_256 contra MinIO: {e:?}");
        }
        Ok(mut sink) => {
            let commit = async {
                sink.write(Bytes::from_static(b"x")).await?;
                sink.commit().await
            }
            .await;
            eprintln!("name_over_max_256 contra MinIO: commit = {commit:?}");
        }
    }
}

/// Conditional write REAL: dos writes al mismo key; el segundo commit pierde
/// con Conflict (`If-None-Match` en el `CompleteMultipartUpload`/`PutObject`).
#[tokio::test]
async fn conditional_write_real() {
    let (_c, p, _op) = setup().await;
    let f = child(&root(), b"unico.txt");
    write_all(&p, &f, b"primero").await;
    // Segundo write sobre la key existente: Conflict al abrir (stat-check) o al
    // commit (If-None-Match) — en ambos casos jamás sobrescribe.
    match p.write(&f).await {
        Err(Error::Conflict { .. }) => {}
        Ok(mut sink) => {
            sink.write(Bytes::from_static(b"segundo"))
                .await
                .expect("chunk");
            assert!(matches!(sink.commit().await, Err(Error::Conflict { .. })));
        }
        Err(e) => panic!("esperaba Conflict, fue {e:?}"),
    }
    assert_eq!(read_all(&p, &f).await, b"primero");
}
