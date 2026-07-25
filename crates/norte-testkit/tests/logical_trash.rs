//! `MemProvider` papelera LÓGICA (#99, cierra deuda H2): destino recuperable
//! `.norte-trash/<id>/payload` + idempotencia bajo fallo transitorio (el
//! efecto ya aplicó y aun así devuelve error), que es lo que el engine
//! `trash_retrying` explota para conservar el `reversal_ref` del undo.

use bytes::Bytes;
use norte_proto::{Error, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;
use norte_vfs::trash::TrashId;

fn vp(w: &str) -> VPath {
    VPath::parse(w).expect("wire de test válido")
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
    assert!(p.stat(&dest.unwrap()).await.is_ok(), "payload recuperable");
}

#[tokio::test]
async fn logical_trash_recovers_the_payload_after_a_transient_post_move_failure() {
    let p = MemProvider::new().with_logical_trash();
    seed_file(&p, "mem:///victim.txt", b"data").await;
    let id = TrashId::new(2000, 3);

    // El movimiento aplica y AUN ASÍ devuelve transitorio (#17): la víctima ya
    // no está pero el `reversal_ref` se perdería sin idempotencia.
    p.faults().ambiguous_mutations(1);
    let first = p.trash(&vp("mem:///victim.txt"), &id).await;
    assert!(
        matches!(first, Err(Error::ProviderUnavailable { retryable: true })),
        "transitorio tras el efecto: {first:?}"
    );
    assert!(
        matches!(p.stat(&vp("mem:///victim.txt")).await, Err(Error::NotFound)),
        "el efecto YA se aplicó (víctima movida)"
    );

    // Reintento con el MISMO id determinista: recupera el payload en vez de
    // dar `NotFound` y perder el destino recuperable.
    let second = p
        .trash(&vp("mem:///victim.txt"), &id)
        .await
        .expect("el reintento recupera");
    assert_eq!(
        second,
        Some(vp("mem:///.norte-trash/2000-3/victim.txt")),
        "reversal_ref preservado"
    );
}

#[tokio::test]
async fn logical_trash_preserves_a_hostile_non_utf8_basename_through_recovery() {
    use futures::StreamExt as _;
    use norte_proto::Segment;

    // Nombre CRUDO no-UTF8 del corpus canónico (surrogate suelto WTF-8): el
    // único codepath que mueve bytes hostiles por el re-key + la recuperación
    // idempotente es `trash_logical` (sftp/S3 son UTF-8-only). Regla CLAUDE.md:
    // las regresiones de path necesitan fixture del corpus canónico.
    let hostile = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "lone_surrogate")
        .expect("corpus lone_surrogate")
        .bytes;
    let p = MemProvider::new().with_logical_trash();
    let victim = MemProvider::root().join(Segment::new(hostile.clone()).expect("segmento"));
    {
        let mut sink = p.write(&victim).await.expect("write");
        sink.write(Bytes::from_static(b"payload"))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    }

    // Transitorio tras el movimiento + reintento: la recuperación compara el
    // payload determinista con bytes hostiles.
    let id = TrashId::new(4000, 9);
    p.faults().ambiguous_mutations(1);
    assert!(p.trash(&victim, &id).await.is_err(), "transitorio");
    let dest = p
        .trash(&victim, &id)
        .await
        .expect("recupera")
        .expect("papelera lógica => Some");

    // El basename del payload conserva los bytes hostiles byte-exactos.
    assert_eq!(
        dest.file_name().map(Segment::as_bytes),
        Some(&hostile[..]),
        "basename hostil byte-exacto"
    );
    // Y el contenido sobrevivió el re-key del subárbol.
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
    // Sin víctima ni payload: `NotFound` genuino, la idempotencia no lo enmascara.
    assert!(matches!(
        p.trash(&vp("mem:///ghost.txt"), &id).await,
        Err(Error::NotFound)
    ));
}
