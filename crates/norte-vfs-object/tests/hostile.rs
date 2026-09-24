//! Containment (threat model §14, ADR 0016 D): impossible names are
//! rejected CLEANLY before touching the network, and a LYING server (keys
//! with an injected `/` or U+FFFD in the listing) cuts the stream with
//! `InvalidPath` instead of letting entries escape the directory.
//!
//! The lying server is raw HTTP (canned responses): an honest s3s would
//! never produce those keys, so the transport layer is faked — the same
//! pattern as norte-connect's fake FTP server.
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
    base.join(Segment::new(name.to_vec()).expect("segment"))
}

/// Provider over an fs Operator (the provider-level cases never reach the network).
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
    // 5 segments of 250 bytes = a 1254-byte key: S3 would reject it midway
    // through an operation with an ambiguous error — here it is InvalidPath
    // BEFORE the network.
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
    // opendal-core's normalize_path does `path.trim()`: "file " would turn
    // INTO "file" SILENTLY (byte corruption, rule 1). Fail-loud until
    // upstream preserves the bytes.
    for name in [&b"file "[..], b" file", b"\tfile", b"file\n"] {
        let f = child(&root(), name);
        assert_eq!(
            p.stat(&f).await.unwrap_err(),
            Error::InvalidPath,
            "{name:?} must be rejected"
        );
        assert!(matches!(p.write(&f).await, Err(Error::InvalidPath)));
    }
}

#[tokio::test]
async fn scheme_ajeno_se_rechaza() {
    let p = fresh_fs();
    let foreign = VPath::parse("ftp://host/f.txt").expect("vpath");
    assert_eq!(p.stat(&foreign).await.unwrap_err(), Error::InvalidPath);
}

// ---------- LYING server ----------

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A canned, LYING S3 HTTP server. Returns `keys` only when the listing
/// asks for a `prefix` starting with `prefix_match` (so it does not
/// pretend EVERYTHING exists and trigger a Conflict before the code under
/// test); HEAD is always 404. Counts mutations (non-list PUT/DELETE/POST)
/// in `mutations`: a safe provider must NOT emit any when the listing is
/// hostile.
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
                    // Only lies for the prefix under attack; the rest
                    // (e.g. the destination's dir probe) = empty.
                    let serve = line.contains(&format!("prefix={prefix_match}"));
                    canned_list(if serve { &keys } else { &[] })
                } else if method == "GET" {
                    "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        .to_string()
                } else {
                    // PUT/DELETE/POST(copy/upload) = an observed mutation.
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

/// A `/` injected into the name (key `dir/../fuera` echoed by the server)
/// tries to escape the listed directory: cuts with `InvalidPath`.
#[tokio::test]
async fn listado_con_slash_inyectado_corta() {
    let p = lying_provider(vec!["dir/../fuera".into(), "dir/normal.txt".into()]).await;
    let d = child(&root(), b"dir");
    let res: Result<Vec<_>, Error> = p.list(&d).await.expect("opens").try_collect().await;
    assert_eq!(res.unwrap_err(), Error::InvalidPath);
}

/// U+FFFD in a name from the server = unrecoverable original bytes (a
/// lossy decoding at some hop): fail-loud rejection, never a corrupt Entry
/// (rule 1).
#[tokio::test]
async fn listado_con_ufffd_corta() {
    let p = lying_provider(vec!["dir/mal\u{FFFD}nombre".into()]).await;
    let d = child(&root(), b"dir");
    let res: Result<Vec<_>, Error> = p.list(&d).await.expect("opens").try_collect().await;
    assert_eq!(res.unwrap_err(), Error::InvalidPath);
}

/// The server's self-entry (`dir/`) does not appear as a child.
#[tokio::test]
async fn listado_filtra_self_entry() {
    let p = lying_provider(vec!["dir/".into(), "dir/ok.txt".into()]).await;
    let d = child(&root(), b"dir");
    let entries: Vec<_> = p
        .list(&d)
        .await
        .expect("opens")
        .try_collect::<Vec<_>>()
        .await
        .expect("stream");
    let names: Vec<Vec<u8>> = entries
        .iter()
        .map(|e| e.path.file_name().expect("name").as_bytes().to_vec())
        .collect();
    assert_eq!(names, vec![b"ok.txt".to_vec()]);
}

/// A key OUTSIDE the requested prefix (no `/`, which the old fallback let
/// through as a phantom Entry) cuts with `InvalidPath`.
#[tokio::test]
async fn listado_key_fuera_de_prefijo_corta() {
    // The server serves these keys when asked for prefix=dir; `dirx`/`otra`
    // do not hang off `dir/`.
    let p = lying_provider(vec!["dirx".into(), "dir/ok.txt".into()]).await;
    let d = child(&root(), b"dir");
    let res: Result<Vec<_>, Error> = p.list(&d).await.expect("opens").try_collect().await;
    assert_eq!(res.unwrap_err(), Error::InvalidPath);
}

/// An EMPTY segment (`dir//x` → name `""` after the delimiter) is a legal
/// S3 key the dir model cannot represent: cuts instead of hiding it.
#[tokio::test]
async fn listado_segmento_vacio_corta() {
    let p = lying_provider(vec!["dir//oculto".into()]).await;
    let d = child(&root(), b"dir");
    let res: Result<Vec<_>, Error> = p.list(&d).await.expect("opens").try_collect().await;
    assert_eq!(res.unwrap_err(), Error::InvalidPath);
}

// ---------- rename against a lying server: ZERO mutations ----------

/// The reviewers' BLOCKER: a dir `rename`'s walk must NOT operate
/// (copy/delete) on keys the server lists OUTSIDE the source prefix. A
/// hostile listing (`otra/x`, `../victima`, `src/file `) must cut with
/// `InvalidPath` having emitted ZERO PUT/DELETE.
#[tokio::test]
async fn rename_dir_con_listado_hostil_no_muta_nada() {
    let mutations = Arc::new(AtomicUsize::new(0));
    // The server serves the hostile listing when asked for prefix=src (the
    // from_dir); the destination's probe (prefix=dst) comes back empty →
    // does not exist → continues.
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
    assert_eq!(err, Error::InvalidPath, "a hostile listing must cut");
    assert_eq!(
        mutations.load(Ordering::SeqCst),
        0,
        "not a single copy/delete could go out toward echoed keys"
    );
}

// ---------- provider-level guards ----------

/// `rename(a, a/b)` (destination inside its own subtree) is cleanly rejected.
#[tokio::test]
async fn rename_dentro_de_su_subarbol_se_rechaza() {
    let p = fresh_fs();
    let r = root();
    let a = child(&r, b"a");
    p.mkdir(&a).await.expect("mkdir a");
    let sub = child(&a, b"b");
    assert_eq!(p.rename(&a, &sub).await.unwrap_err(), Error::InvalidPath);
    // and to itself.
    assert_eq!(p.rename(&a, &a).await.unwrap_err(), Error::InvalidPath);
}

/// A trailing NBSP (Unicode `White_Space` that `str::trim` trims but
/// `is_ascii_whitespace` does not): shields the anti-trim predicate against
/// an ASCII-only regression (opendal's `normalize_path` uses `str::trim`).
#[tokio::test]
async fn nombre_con_nbsp_final_se_rechaza() {
    let p = fresh_fs();
    let f = child(&root(), "file\u{A0}".as_bytes());
    assert_eq!(p.stat(&f).await.unwrap_err(), Error::InvalidPath);
    assert!(matches!(p.write(&f).await, Err(Error::InvalidPath)));
}

/// `copy_native` with a DIRECTORY source → `TypeMismatch` (`copy_native` is
/// single-object; the engine copies trees leaf by leaf). Over services-fs
/// because s3s-fs lies on a directory path's HEAD (issue #50).
#[tokio::test]
async fn copy_native_origen_dir_es_typemismatch() {
    let p = fresh_fs();
    let r = root();
    let d = child(&r, b"undir");
    p.mkdir(&d).await.expect("mkdir");
    common::write_all(&p, &child(&d, b"hijo.txt"), b"x").await;
    let dst = child(&r, b"destino");
    match p.copy_native(&d, &dst).await {
        Some(Err(Error::Conflict {
            conflict: norte_proto::ConflictKind::TypeMismatch,
        })) => {}
        other => panic!("copy_native of a dir should have given TypeMismatch, was {other:?}"),
    }
}

/// `copy_native` to a DESTINATION that is a directory → `Conflict`: the
/// copy's `If-None-Match` does not see the dir, the guard is
/// `ensure_absent`. Over services-fs (reliable dirs).
#[tokio::test]
async fn copy_native_destino_dir_es_conflict() {
    let p = fresh_fs();
    let r = root();
    let src = child(&r, b"origen.bin");
    common::write_all(&p, &src, b"x").await;
    let dst = child(&r, b"dst-dir");
    p.mkdir(&dst).await.expect("mkdir dst");
    match p.copy_native(&src, &dst).await {
        Some(Err(Error::Conflict { .. })) => {}
        other => {
            panic!("copy_native onto a destination dir should have given Conflict, was {other:?}")
        }
    }
}
