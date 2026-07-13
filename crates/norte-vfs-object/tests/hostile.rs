//! Contención (threat model §14, ADR 0016 D): nombres imposibles se rechazan
//! LIMPIO antes de tocar la red, y un servidor MENTIROSO (keys con `/`
//! inyectado o U+FFFD en el listado) corta el stream con `InvalidPath` en vez
//! de dejar escapar entradas fuera del directorio.
//!
//! El servidor mentiroso es HTTP crudo (respuestas enlatadas): un s3s
//! honesto jamás produciría esas keys, así que se falsifica la capa de
//! transporte — mismo patrón que el servidor FTP falso de norte-connect.
#![cfg(target_os = "linux")]

mod common;

use futures::TryStreamExt;
use norte_proto::{Authority, Error, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs_object::ObjectProvider;

fn root() -> VPath {
    ObjectProvider::root(
        "s3",
        Authority::new(common::TEST_BUCKET).expect("authority"),
    )
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(Segment::new(name.to_vec()).expect("segmento"))
}

/// Provider sobre un Operator fs (los casos provider-level no llegan a red).
fn fresh_fs() -> ObjectProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().join("root");
    let atomic = dir.path().join("staging");
    std::fs::create_dir_all(&base).expect("root");
    std::fs::create_dir_all(&atomic).expect("staging");
    let op = common::fs_operator(&base, &atomic);
    std::mem::forget(dir);
    ObjectProvider::new(op, "s3")
}

#[tokio::test]
async fn nombre_no_utf8_se_rechaza_limpio() {
    let p = fresh_fs();
    let f = child(&root(), b"latin1-\xe9.txt");
    assert_eq!(p.stat(&f).await.unwrap_err(), Error::InvalidPath);
    assert!(matches!(p.write(&f).await, Err(Error::InvalidPath)));
    assert_eq!(p.mkdir(&f).await.unwrap_err(), Error::InvalidPath);
}

#[tokio::test]
async fn key_de_mas_de_1024_bytes_se_rechaza_upfront() {
    let p = fresh_fs();
    // 5 segmentos de 250 bytes = 1254 de key: S3 la rechazaría a media
    // operación con un error ambiguo — aquí es InvalidPath ANTES de la red.
    let mut f = root();
    for _ in 0..5 {
        f = child(&f, "x".repeat(250).as_bytes());
    }
    assert_eq!(p.stat(&f).await.unwrap_err(), Error::InvalidPath);
    assert!(matches!(p.write(&f).await, Err(Error::InvalidPath)));
}

#[tokio::test]
async fn whitespace_en_extremos_se_rechaza_no_se_renombra() {
    let p = fresh_fs();
    // El normalize_path de opendal-core hace `path.trim()`: "file " se
    // convertiría EN SILENCIO en "file" (corrupción de bytes, regla 1).
    // Fail-loud hasta que upstream preserve los bytes.
    for name in [&b"file "[..], b" file", b"\tfile", b"file\n"] {
        let f = child(&root(), name);
        assert_eq!(
            p.stat(&f).await.unwrap_err(),
            Error::InvalidPath,
            "{name:?} debe rechazarse"
        );
        assert!(matches!(p.write(&f).await, Err(Error::InvalidPath)));
    }
}

#[tokio::test]
async fn scheme_ajeno_se_rechaza() {
    let p = fresh_fs();
    let ajeno = VPath::parse("ftp://host/f.txt").expect("vpath");
    assert_eq!(p.stat(&ajeno).await.unwrap_err(), Error::InvalidPath);
}

// ---------- servidor MENTIROSO ----------

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Servidor HTTP S3 enlatado y MENTIROSO. Devuelve `keys` solo cuando el
/// listado pide un `prefix` que empieza por `prefix_match` (para no fingir
/// que TODO existe y disparar un Conflict antes del código bajo prueba);
/// HEAD siempre 404. Cuenta las mutaciones (PUT/DELETE/POST no-list) en
/// `mutations`: un provider seguro NO debe emitir ninguna cuando el listado
/// es hostil.
async fn lying_server(
    keys: Vec<String>,
    prefix_match: &'static str,
    mutations: Arc<AtomicUsize>,
) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let keys = keys.clone();
            let mutations = Arc::clone(&mutations);
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let line = req.lines().next().unwrap_or("");
                let method = line.split(' ').next().unwrap_or("");
                let is_list = line.contains("list-type=2");
                let response = if method == "HEAD" {
                    "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        .to_string()
                } else if method == "GET" && is_list {
                    // Solo miente para el prefijo bajo ataque; el resto
                    // (p. ej. el sondeo de dir del destino) = vacío.
                    let serve = line.contains(&format!("prefix={prefix_match}"));
                    canned_list(if serve { &keys } else { &[] })
                } else if method == "GET" {
                    "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        .to_string()
                } else {
                    // PUT/DELETE/POST(copy/upload) = mutación observada.
                    mutations.fetch_add(1, Ordering::SeqCst);
                    "HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string()
                };
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    addr
}

fn canned_list(keys: &[String]) -> String {
    use std::fmt::Write as _;
    let mut contents = String::new();
    for k in keys {
        let _ = write!(
            contents,
            "<Contents><Key>{k}</Key><Size>1</Size>\
             <LastModified>2026-01-01T00:00:00.000Z</LastModified>\
             <ETag>&quot;0&quot;</ETag></Contents>"
        );
    }
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <ListBucketResult><Name>{}</Name>\
         <KeyCount>{}</KeyCount><MaxKeys>1000</MaxKeys>\
         <IsTruncated>false</IsTruncated>{contents}</ListBucketResult>",
        common::TEST_BUCKET,
        keys.len(),
    );
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/xml\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

async fn lying_provider(keys: Vec<String>) -> ObjectProvider {
    let addr = lying_server(keys, "dir", Arc::new(AtomicUsize::new(0))).await;
    ObjectProvider::new(common::s3_operator(addr), "s3")
}

/// Un `/` inyectado en el nombre (key `dir/../fuera` ecoada por el servidor)
/// busca escapar del directorio listado: corta con `InvalidPath`.
#[tokio::test]
async fn listado_con_slash_inyectado_corta() {
    let p = lying_provider(vec!["dir/../fuera".into(), "dir/normal.txt".into()]).await;
    let d = child(&root(), b"dir");
    let res: Result<Vec<_>, Error> = p.list(&d).await.expect("abre").try_collect().await;
    assert_eq!(res.unwrap_err(), Error::InvalidPath);
}

/// U+FFFD en un nombre del servidor = bytes originales irrecuperables
/// (decodificación lossy en algún salto): rechazo fail-loud, jamás un Entry
/// corrupto (regla 1).
#[tokio::test]
async fn listado_con_ufffd_corta() {
    let p = lying_provider(vec!["dir/mal\u{FFFD}nombre".into()]).await;
    let d = child(&root(), b"dir");
    let res: Result<Vec<_>, Error> = p.list(&d).await.expect("abre").try_collect().await;
    assert_eq!(res.unwrap_err(), Error::InvalidPath);
}

/// La self-entry del servidor (`dir/`) no aparece como hijo.
#[tokio::test]
async fn listado_filtra_self_entry() {
    let p = lying_provider(vec!["dir/".into(), "dir/ok.txt".into()]).await;
    let d = child(&root(), b"dir");
    let entries: Vec<_> = p
        .list(&d)
        .await
        .expect("abre")
        .try_collect::<Vec<_>>()
        .await
        .expect("stream");
    let names: Vec<Vec<u8>> = entries
        .iter()
        .map(|e| e.path.file_name().expect("nombre").as_bytes().to_vec())
        .collect();
    assert_eq!(names, vec![b"ok.txt".to_vec()]);
}

/// Una key FUERA del prefijo pedido (sin `/`, que el fallback viejo dejaba
/// pasar como Entry fantasma) corta con `InvalidPath`.
#[tokio::test]
async fn listado_key_fuera_de_prefijo_corta() {
    // El server sirve estas keys al pedir prefix=dir; `dirx`/`otra` no
    // cuelgan de `dir/`.
    let p = lying_provider(vec!["dirx".into(), "dir/ok.txt".into()]).await;
    let d = child(&root(), b"dir");
    let res: Result<Vec<_>, Error> = p.list(&d).await.expect("abre").try_collect().await;
    assert_eq!(res.unwrap_err(), Error::InvalidPath);
}

/// Un segmento VACÍO (`dir//x` → nombre `""` tras el delimiter) es una key S3
/// legal que el modelo de dirs no representa: corta en vez de ocultarla.
#[tokio::test]
async fn listado_segmento_vacio_corta() {
    let p = lying_provider(vec!["dir//oculto".into()]).await;
    let d = child(&root(), b"dir");
    let res: Result<Vec<_>, Error> = p.list(&d).await.expect("abre").try_collect().await;
    assert_eq!(res.unwrap_err(), Error::InvalidPath);
}

// ---------- rename contra servidor mentiroso: CERO mutaciones ----------

/// El BLOCKER de los reviewers: el walk de `rename` de dir NO debe operar
/// (copy/delete) sobre keys que el servidor liste FUERA del prefijo origen.
/// Un listado hostil (`otra/x`, `../victima`, `src/file `) debe cortar con
/// `InvalidPath` habiendo emitido CERO PUT/DELETE.
#[tokio::test]
async fn rename_dir_con_listado_hostil_no_muta_nada() {
    let mutations = Arc::new(AtomicUsize::new(0));
    // El servidor sirve el listado hostil al pedir prefix=src (el from_dir);
    // el sondeo del destino (prefix=dst) va vacío → no existe → sigue.
    let addr = lying_server(
        vec![
            "src/legit.txt".into(),
            "otra-rama/secreto".into(),
            "../victima".into(),
        ],
        "src",
        Arc::clone(&mutations),
    )
    .await;
    let p = ObjectProvider::new(common::s3_operator(addr), "s3");
    let from = child(&root(), b"src");
    let to = child(&root(), b"dst");
    let err = p.rename(&from, &to).await.unwrap_err();
    assert_eq!(err, Error::InvalidPath, "listado hostil debe cortar");
    assert_eq!(
        mutations.load(Ordering::SeqCst),
        0,
        "ni un solo copy/delete pudo salir hacia keys ecoadas"
    );
}

// ---------- guards provider-level ----------

/// `rename(a, a/b)` (destino dentro del propio subárbol) se rechaza limpio.
#[tokio::test]
async fn rename_dentro_de_su_subarbol_se_rechaza() {
    let p = fresh_fs();
    let r = root();
    let a = child(&r, b"a");
    p.mkdir(&a).await.expect("mkdir a");
    let sub = child(&a, b"b");
    assert_eq!(p.rename(&a, &sub).await.unwrap_err(), Error::InvalidPath);
    // y a sí mismo.
    assert_eq!(p.rename(&a, &a).await.unwrap_err(), Error::InvalidPath);
}

/// NBSP final (`White_Space` Unicode que `str::trim` recorta pero
/// `is_ascii_whitespace` no): blinda el predicado anti-trim contra una
/// regresión a solo-ASCII (`normalize_path` de opendal usa `str::trim`).
#[tokio::test]
async fn nombre_con_nbsp_final_se_rechaza() {
    let p = fresh_fs();
    let f = child(&root(), "file\u{A0}".as_bytes());
    assert_eq!(p.stat(&f).await.unwrap_err(), Error::InvalidPath);
    assert!(matches!(p.write(&f).await, Err(Error::InvalidPath)));
}
