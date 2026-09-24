//! `MemProvider` LOGICAL trash (#99, closes debt H2): recoverable destination
//! `.norte-trash/<id>/payload` + idempotency under transient failure (the
//! effect already applied and it still returns an error), which is what the
//! engine's `trash_retrying` exploits to keep undo's `reversal_ref`.

use bytes::Bytes;
use norte_proto::{Error, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;
use norte_vfs::trash::TrashId;

fn vp(w: &str) -> VPath {
    VPath::parse(w).expect("valid test wire")
}

async fn seed_file(p: &MemProvider, wire: &str, body: &[u8]) {
    let mut sink = p.write(&vp(wire)).await.expect("write");
    sink.write(Bytes::copy_from_slice(body))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

#[tokio::test]
async fn logical_trash_moves_victim_and_returns_the_recoverable_payload() {
    let p = MemProvider::new().with_logical_trash();
    seed_file(&p, "mem:///victim.txt", b"data").await;

    let id = TrashId::new(1000, 7);
    let dest = p.trash(&vp("mem:///victim.txt"), &id).await.expect("trash");

    assert_eq!(dest, Some(vp("mem:///.norte-trash/1000-7/victim.txt")));
    assert!(matches!(
        p.stat(&vp("mem:///victim.txt")).await,
        Err(Error::NotFound)
    ));
    assert!(p.stat(&dest.unwrap()).await.is_ok(), "recoverable payload");
}

#[tokio::test]
async fn logical_trash_recovers_the_payload_after_a_transient_post_move_failure() {
    let p = MemProvider::new().with_logical_trash();
    seed_file(&p, "mem:///victim.txt", b"data").await;
    let id = TrashId::new(2000, 3);

    // The move applies and STILL returns transient (#17): the victim is
    // already gone but the `reversal_ref` would be lost without idempotency.
    p.faults().ambiguous_mutations(1);
    let first = p.trash(&vp("mem:///victim.txt"), &id).await;
    assert!(
        matches!(first, Err(Error::ProviderUnavailable { retryable: true })),
        "transient after the effect: {first:?}"
    );
    assert!(
        matches!(p.stat(&vp("mem:///victim.txt")).await, Err(Error::NotFound)),
        "the effect has ALREADY been applied (victim moved)"
    );

    // Retry with the SAME deterministic id: recovers the payload instead of
    // giving `NotFound` and losing the recoverable destination.
    let second = p
        .trash(&vp("mem:///victim.txt"), &id)
        .await
        .expect("the retry recovers");
    assert_eq!(
        second,
        Some(vp("mem:///.norte-trash/2000-3/victim.txt")),
        "reversal_ref preserved"
    );
}

#[tokio::test]
async fn logical_trash_preserves_a_hostile_non_utf8_basename_through_recovery() {
    use futures::StreamExt as _;
    use norte_proto::Segment;

    // A RAW non-UTF8 name from the canonical corpus (a lone WTF-8
    // surrogate): the only codepath that moves hostile bytes through the
    // re-key + idempotent recovery is `trash_logical` (sftp/S3 are
    // UTF-8-only). CLAUDE.md rule: path regressions need a canonical
    // corpus fixture.
    let hostile = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "lone_surrogate")
        .expect("corpus lone_surrogate")
        .bytes;
    let p = MemProvider::new().with_logical_trash();
    let victim = MemProvider::root().join(Segment::new(hostile.clone()).expect("segment"));
    {
        let mut sink = p.write(&victim).await.expect("write");
        sink.write(Bytes::from_static(b"payload"))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }

    // Transient after the move + retry: recovery compares the deterministic
    // payload with hostile bytes.
    let id = TrashId::new(4000, 9);
    p.faults().ambiguous_mutations(1);
    assert!(p.trash(&victim, &id).await.is_err(), "transient");
    let dest = p
        .trash(&victim, &id)
        .await
        .expect("recovers")
        .expect("logical trash => Some");

    // The payload's basename keeps the hostile bytes byte-exact.
    assert_eq!(
        dest.file_name().map(Segment::as_bytes),
        Some(&hostile[..]),
        "byte-exact hostile basename"
    );
    // And the content survived the subtree's re-key.
    let mut stream = p.read(&dest, None).await.expect("read payload");
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk.expect("chunk"));
    }
    assert_eq!(buf, b"payload");
}

#[tokio::test]
async fn logical_trash_of_a_never_existing_victim_is_not_found() {
    let p = MemProvider::new().with_logical_trash();
    let id = TrashId::new(3000, 1);
    // No victim and no payload: genuine `NotFound`, idempotency does not mask it.
    assert!(matches!(
        p.trash(&vp("mem:///ghost.txt"), &id).await,
        Err(Error::NotFound)
    ));
}
