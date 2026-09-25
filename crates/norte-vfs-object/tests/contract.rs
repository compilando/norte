//! `provider_contract!` against opendal's `services-fs` (ADR 0016 J): the
//! SAME suite Mem/Local/Sftp/Ftp pass, against the provider's full logic
//! (key validation, dir model, sink, error mapping) without HTTP. The
//! S3-specific bits this harness does not exercise (multipart, conditional
//! write, real delimiter) live in tests/s3.rs against s3s-fs, and the
//! nightly job (`reals3`) validates against `MinIO`.
//!
//! Linux-only (like sftp/ftp): the harness relies on the host's FS and is
//! only faithful on POSIX (case-sensitive, byte-preserving)… with one
//! DELIBERATE asymmetry: S3 keys are UTF-8-only, so the corpus's non-UTF8
//! fixtures are cleanly rejected by the provider (a contract skip), they
//! never reach the FS.
#![cfg(target_os = "linux")]

mod common;

use norte_proto::Authority;
use norte_vfs_object::ObjectProvider;

/// A fresh provider over a tempdir via `services-fs` (with
/// `atomic_write_dir` OUTSIDE the listable root: a writer's `.tmp` is not an
/// entry).
fn fresh() -> ObjectProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("root");
    let atomic = dir.path().join("staging");
    std::fs::create_dir_all(&root).expect("root");
    std::fs::create_dir_all(&atomic).expect("staging");
    let op = common::fs_operator(&root, &atomic);
    // The tempdir lives as long as the provider (ephemeral tests; the OS cleans /tmp).
    std::mem::forget(dir);
    ObjectProvider::new(op, "s3")
}

fn hostile_names() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        // The name_max fixtures (255/256 bytes) are LEGAL S3 keys (1024
        // limit, no per-segment cap) this FS harness cannot store: POSIX
        // NAME_MAX = 255 and opendal's atomic_write_dir adds ".XXXXXXXX"
        // (9 bytes) to the tempfile → effective cap 246. A harness
        // limitation, not the provider's — fs blows up AFTER the open and
        // the contract demands a clean rejection or success. The nightly
        // job covers them against real `MinIO` (tests/reals3.rs).
        .filter(|bytes| bytes.len() <= 246)
        .collect()
}

norte_vfs::provider_contract! {
    mod object_fs,
    factory: fresh(),
    root: ObjectProvider::root("s3", Authority::new("norte-test").expect("valid authority")),
    hostile_names: hostile_names(),
}

/// The same provider with the logical trash TURNED ON (ADR 0019).
fn fresh_with_trash() -> ObjectProvider {
    fresh().with_logical_trash(true)
}

// The whole suite, again, with the logical trash set (#168).
//
// It is not duplication: it is the ONLY configuration in which the
// contract branch that says "the destination exists and restores" runs —
// the one that demands `trash()` name what it buries, that `reversal_ref`
// be `Some`, and that `restore_from` return the exact node with its bytes
// and its name, non-UTF8 names included.
//
// This pass exists because its absence already cost a real bug: this
// provider returned `Some` from `trash()` and never overrode
// `trash_restorable()`, which defaults to `false`. A sync against S3 with
// the trash on would have been planned ENTIRELY as irreversible —every
// step, copies included— throwing away its `reversal_ref`, while the trash
// was perfectly restorable. The plan would have told the human "none of
// this can be undone" and then would have buried things somewhere it knew
// how to reach.
norte_vfs::provider_contract! {
    mod object_fs_trash,
    factory: fresh_with_trash(),
    root: ObjectProvider::root("s3", Authority::new("norte-test").expect("valid authority")),
    hostile_names: hostile_names(),
}

// ---------- s3 attrs (#108 block 2) ----------

#[tokio::test]
async fn attrs_s3_etag_y_content_type() {
    use futures::StreamExt;
    use norte_proto::{AttrValue, Segment};
    use norte_vfs::{AttrRequest, ListOptions, Provider};

    let p = fresh();
    let root = ObjectProvider::root("s3", Authority::new("norte-test").expect("valid authority"));
    let f = root.join(Segment::new(b"o.txt".to_vec()).expect("valid segment"));
    {
        let mut sink = p.write(&f).await.expect("write opens");
        norte_vfs::ByteSink::write(&mut *sink, bytes::Bytes::from_static(b"x"))
            .await
            .expect("chunk goes in");
        sink.commit().await.expect("commit publishes");
    }
    let opt = ListOptions {
        attrs: AttrRequest::sanitized(["s3.etag", "s3.content_type"].map(str::to_owned)),
    };
    // services-fs may not give an etag/content_type: if present, they are
    // capped Text (the contract pins the shape; the REAL value is covered
    // by the nightly MinIO job).
    let e = p.stat_with(&f, &opt).await.expect("stat_with");
    for id in ["s3.etag", "s3.content_type"] {
        if let Some(v) = e.attrs.get(id) {
            let AttrValue::Text(s) = v else {
                panic!("{id} must be Text, was {v:?}");
            };
            assert!(s.len() <= norte_proto::ATTR_TEXT_MAX);
        }
    }
    // list_with: same rules per entry, and never an unrequested id.
    let mut s = p.list_with(&root, &opt).await.expect("list_with");
    while let Some(e) = s.next().await {
        let e = e.expect("entry");
        for id in e.attrs.keys() {
            assert!(opt.attrs.wants(id), "unrequested id: {id}");
        }
    }
    // No request → nothing.
    assert!(p.stat(&f).await.expect("stat").attrs.is_empty());
}
