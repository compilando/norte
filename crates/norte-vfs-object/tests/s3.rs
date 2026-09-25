//! The S3-SPECIFIC suite against the in-process s3s-fs server (ADR 0016 J):
//! what the contract's fs harness does not exercise — real multipart,
//! pre-commit invisibility via multipart, conditional write
//! (If-None-Match) in the race window, `ListObjectsV2`'s delimiter and
//! marker-less prefixes. Only what the 7b spike measured as FAITHFUL on
//! s3s-fs; empty-dir markers, long keys AND EVERYTHING that touches dirs
//! goes to the nightly job (real `MinIO`): s3s-fs's `HeadObject` on a path
//! that is a directory on its fs returns 500 (real S3: 404), which
//! poisons the provider's file→dir probe. Dir semantics are covered by the
//! contract over services-fs.
#![cfg(target_os = "linux")]

mod common;

use bytes::Bytes;
use futures::TryStreamExt;
use norte_proto::{Authority, Error, Segment, VPath};
use norte_vfs::Provider;
use norte_vfs_object::ObjectProvider;

/// Provider + the raw Operator (to SEED the bucket "from outside", as
/// another tool would: s3s-fs loses empty-dir markers, so parents are
/// populated with objects, not with mkdir).
async fn fresh() -> (ObjectProvider, norte_vfs_object::Operator) {
    let dir = tempfile::tempdir().expect("tempdir");
    let addr = common::start_s3s(dir.path()).await;
    std::mem::forget(dir);
    let op = common::s3_operator(addr);
    (ObjectProvider::new(op.clone(), "s3"), op)
}

fn root() -> VPath {
    ObjectProvider::root(
        "s3",
        Authority::new(common::TEST_BUCKET).expect("authority"),
    )
}

fn child(base: &VPath, name: &[u8]) -> VPath {
    base.join(Segment::new(name.to_vec()).expect("segment"))
}

use common::{read_all, write_all};

// ---------- copy_native (phase 7c) ----------

/// `copy_native` = server-side `CopyObject`: byte-exact copy of a hostile
/// name without reading+rewriting.
#[tokio::test]
async fn copy_native_byte_exact_hostile_name() {
    let (p, _op) = fresh().await;
    let r = root();
    let src = child(&r, "ñ é+%20.bin".as_bytes());
    write_all(&p, &src, b"contenido-servidor").await;
    let dst = child(&r, b"copia.bin");
    match p.copy_native(&src, &dst).await {
        Some(Ok(())) => {}
        other => panic!("copy_native should have copied, was {other:?}"),
    }
    assert_eq!(
        read_all(&p, &dst).await.expect("read"),
        b"contenido-servidor"
    );
    // the source stays intact (copy, not move).
    assert_eq!(
        read_all(&p, &src).await.expect("read"),
        b"contenido-servidor"
    );
}

/// Existing destination → `Conflict`, never a silent overwrite.
#[tokio::test]
async fn copy_native_existing_destination_is_conflict() {
    let (p, _op) = fresh().await;
    let r = root();
    let src = child(&r, b"a.bin");
    let dst = child(&r, b"b.bin");
    write_all(&p, &src, b"origen").await;
    write_all(&p, &dst, b"NO-pisar").await;
    match p.copy_native(&src, &dst).await {
        Some(Err(Error::Conflict { .. })) => {}
        other => {
            panic!("copy_native onto an existing one should have given Conflict, was {other:?}")
        }
    }
    assert_eq!(read_all(&p, &dst).await.expect("read"), b"NO-pisar");
}

/// Absent source → `NotFound` (implemented branch, no contract coverage).
#[tokio::test]
async fn copy_native_source_absent_es_not_found() {
    let (p, _op) = fresh().await;
    let r = root();
    let src = child(&r, b"no-existe.bin");
    let dst = child(&r, b"destino.bin");
    assert_eq!(p.copy_native(&src, &dst).await, Some(Err(Error::NotFound)));
}

/// Destination whose parent does NOT exist → `NotFound` (same policy as `write`).
#[tokio::test]
async fn copy_native_missing_destination_parent_is_not_found() {
    let (p, _op) = fresh().await;
    let r = root();
    let src = child(&r, b"origen.bin");
    write_all(&p, &src, b"x").await;
    let dst = child(&child(&r, b"dir-inexistente"), b"destino.bin");
    assert_eq!(p.copy_native(&src, &dst).await, Some(Err(Error::NotFound)));
}

// (copy_native of a directory source → TypeMismatch is tested in hostile.rs
// over services-fs: s3s-fs returns 500 on a directory path's HEAD, not 404.)

/// Real multipart: >8 MiB chunk → `CreateMultipartUpload` + `UploadPart` +
/// `CompleteMultipartUpload` underneath; byte-exact roundtrip.
#[tokio::test]
async fn multipart_roundtrip_byte_exact() {
    let (p, _op) = fresh().await;
    let f = child(&root(), b"grande.bin");
    let big: Vec<u8> = (0..12 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    let mut sink = p.write(&f).await.expect("write");
    for c in big.chunks(1024 * 1024) {
        sink.write(Bytes::copy_from_slice(c)).await.expect("chunk");
    }
    sink.commit().await.expect("commit");
    assert_eq!(read_all(&p, &f).await.expect("read"), big);
}

/// Pre-commit invisibility on S3 is given by the multipart ITSELF: parts
/// already uploaded (>8 MiB written) and the key still does not exist;
/// `abort` = `AbortMultipartUpload`, no trace.
#[tokio::test]
async fn multipart_invisible_until_commit_and_abort_leaves_no_trace() {
    let (p, _op) = fresh().await;
    let f = child(&root(), b"invisible.bin");
    let mut sink = p.write(&f).await.expect("write");
    sink.write(Bytes::from(vec![7u8; 9 * 1024 * 1024]))
        .await
        .expect("chunk that forces multipart");
    assert_eq!(
        p.stat(&f).await.unwrap_err(),
        Error::NotFound,
        "uploaded parts do NOT publish the key"
    );
    sink.abort().await.expect("abort");
    assert_eq!(p.stat(&f).await.unwrap_err(), Error::NotFound);
}

/// The create-new race window: TWO sinks opened over the same key (both
/// passed the stat-check); the second commit loses with Conflict —
/// If-None-Match travels in the commit (race-free on honest servers, a
/// better guarantee than ftp's TOCTOU).
#[tokio::test]
async fn conditional_write_closes_the_race_window() {
    let (p, _op) = fresh().await;
    let f = child(&root(), b"conflicto.txt");
    let mut sink_a = p.write(&f).await.expect("write a");
    let mut sink_b = p.write(&f).await.expect("write b (key does not exist yet)");
    sink_a.write(Bytes::from_static(b"gana")).await.expect("a");
    sink_b
        .write(Bytes::from_static(b"pierde"))
        .await
        .expect("b");
    sink_a.commit().await.expect("commit a");
    match sink_b.commit().await {
        Err(Error::Conflict { .. }) => {}
        other => panic!("expected Conflict from If-None-Match, was {other:?}"),
    }
    assert_eq!(read_all(&p, &f).await.expect("read"), b"gana");
}

/// And the simple case: write over an existing key = Conflict ON OPEN.
#[tokio::test]
async fn write_over_existing_conflicts_on_open() {
    let (p, _op) = fresh().await;
    let f = child(&root(), b"ocupado.txt");
    write_all(&p, &f, b"1").await;
    match p.write(&f).await {
        Err(Error::Conflict { .. }) => {}
        Err(e) => panic!("expected Conflict, was {e:?}"),
        Ok(_) => panic!("expected Conflict, the write opened"),
    }
}

/// Names S3 allows and the fs harness too — byte-exact via the real S3 API
/// (INTERIOR spaces, unicode, trailing dot, `+`, a literal `%20`).
#[tokio::test]
async fn names_s3_byte_exactos() {
    let (p, _op) = fresh().await;
    let r = root();
    for name in [
        "con espacio.txt".as_bytes(),
        "ñé—😀.txt".as_bytes(),
        "punto-final.".as_bytes(),
        "a+b.txt".as_bytes(),
        "ya%20codificado.txt".as_bytes(),
    ] {
        let f = child(&r, name);
        write_all(&p, &f, name).await;
        assert_eq!(
            read_all(&p, &f).await.expect("read"),
            name,
            "roundtrip of {name:?}"
        );
    }
    let listed: Vec<Vec<u8>> = p
        .list(&r)
        .await
        .expect("list")
        .try_collect::<Vec<_>>()
        .await
        .expect("stream")
        .into_iter()
        .map(|e| e.path.file_name().expect("name").as_bytes().to_vec())
        .collect();
    for name in [
        "con espacio.txt",
        "ñé—😀.txt",
        "punto-final.",
        "a+b.txt",
        "ya%20codificado.txt",
    ] {
        assert!(
            listed.contains(&name.as_bytes().to_vec()),
            "{name:?} byte-exact in the listing"
        );
    }
}

/// pread: mid range, tail, past-EOF (empty) and clamped len, against real
/// HTTP range semantics.
#[tokio::test]
async fn read_range_semantic_pread() {
    use norte_proto::ByteRange;
    let (p, _op) = fresh().await;
    let f = child(&root(), b"rango.bin");
    write_all(&p, &f, b"0123456789").await;
    let leer = |r: Option<ByteRange>| {
        let p = &p;
        let f = f.clone();
        async move {
            let mut s = p.read(&f, r).await.expect("read");
            let mut out = Vec::new();
            while let Some(c) = s.try_next().await.expect("chunk") {
                out.extend_from_slice(&c);
            }
            out
        }
    };
    assert_eq!(
        leer(Some(ByteRange {
            offset: 2,
            len: Some(3)
        }))
        .await,
        b"234"
    );
    assert_eq!(
        leer(Some(ByteRange {
            offset: 8,
            len: None
        }))
        .await,
        b"89"
    );
    assert_eq!(
        leer(Some(ByteRange {
            offset: 100,
            len: Some(4)
        }))
        .await,
        b""
    );
    assert_eq!(
        leer(Some(ByteRange {
            offset: 7,
            len: Some(100)
        }))
        .await,
        b"789"
    );
}
