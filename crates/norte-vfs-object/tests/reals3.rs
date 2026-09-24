//! NIGHTLY: the object provider against a REAL S3 server (`MinIO`) via
//! testcontainers (ADR 0016 J, spec §12). Outside the PR gate (requires
//! Docker): run by `just it-remote` from the nightly workflow.
//!
//! Covers what the in-process harness does NOT give (issue #50): dir
//! semantics (markers/prefix-probe, which s3s-fs breaks), long keys
//! (>the host FS's `NAME_MAX`), `copy_native` with a real `If-None-Match`
//! (s3s-fs ignores it) and the conditional write `MinIO` DOES validate.
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
    base.join(Segment::new(name.to_vec()).expect("segment"))
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

/// A `MinIO` container (bitnami: auto-creates the bucket via
/// `MINIO_DEFAULT_BUCKETS`) plus the provider and the raw `Operator`. The
/// Operator seeds keys "from outside" (like another tool): a marker-less
/// prefix the provider itself would not create because of its
/// parent-exists check. One test per container.
async fn setup() -> (
    testcontainers::ContainerAsync<GenericImage>,
    ObjectProvider,
    Operator,
) {
    // bitnamilegacy: bitnami moved its public images to this namespace in
    // 2025 (auto-creates the bucket with MINIO_DEFAULT_BUCKETS, which the
    // official minio/minio image does not support). The WaitFor is lax:
    // setup() retries the list until the bucket exists.
    let container = GenericImage::new("bitnamilegacy/minio", "latest")
        // MinIO's banner goes to STDERR (bitnami's setup goes to stdout).
        .with_wait_for(WaitFor::message_on_stderr("MinIO Object Storage Server"))
        .with_env_var("MINIO_ROOT_USER", AK)
        .with_env_var("MINIO_ROOT_PASSWORD", SK)
        .with_env_var("MINIO_DEFAULT_BUCKETS", BUCKET)
        .start()
        .await
        .expect("start MinIO");
    let port = container.get_host_port_ipv4(9000).await.expect("port");
    opendal::install_default();
    let builder = opendal::services::S3::default()
        .bucket(BUCKET)
        .region("us-east-1")
        .endpoint(&format!("http://127.0.0.1:{port}"))
        .access_key_id(AK)
        .secret_access_key(SK)
        .disable_config_load()
        .disable_ec2_metadata();
    // The bucket can take a moment to exist after startup: retry.
    let op = Operator::new(builder).expect("operator");
    for _ in 0..40 {
        if op.list_with("").limit(1).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    (container, ObjectProvider::new(op.clone(), "s3"), op)
}

/// Dir semantics against real S3: mkdir (marker) + stat, marker-less prefix
/// = Dir, `remove` of a non-empty dir = Conflict, subtree `rename`.
#[tokio::test]
async fn dirs_markers_y_rename() {
    let (_c, p, op) = setup().await;
    let r = root();
    // mkdir + stat of the marker (an EMPTY dir: the case s3s-fs does not
    // get right).
    let d = child(&r, b"undir");
    p.mkdir(&d).await.expect("mkdir");
    assert_eq!(p.stat(&d).await.expect("stat dir").kind, EntryKind::Dir);
    // A file inside; a non-empty dir does not delete.
    write_all(&p, &child(&d, b"f.txt"), b"x").await;
    assert!(matches!(
        p.remove(&d).await,
        Err(Error::Conflict {
            conflict: ConflictKind::TypeMismatch
        })
    ));
    // A prefix WITHOUT a marker (seeded by the raw Operator, like another
    // tool would): the provider would not create it because of its
    // parent-exists check.
    op.write("prefijo/hijo.txt", b"y".to_vec())
        .await
        .expect("seed prefix");
    assert_eq!(
        p.stat(&child(&r, b"prefijo"))
            .await
            .expect("stat prefix")
            .kind,
        EntryKind::Dir
    );
    // Subtree rename: byte-exact content at the destination, source gone.
    let dst = child(&r, b"movido");
    p.rename(&d, &dst).await.expect("rename dir");
    assert_eq!(read_all(&p, &child(&dst, b"f.txt")).await, b"x");
    assert_eq!(
        p.stat(&child(&d, b"f.txt")).await.unwrap_err(),
        Error::NotFound
    );
}

/// Long keys the in-process fs harness does not cover (the host's
/// `NAME_MAX` + `atomic_write_dir`'s `.XXXXXXXX` = a 246 cap). `MinIO` (an
/// FS backend) limits each COMPONENT to 255 bytes like a real `NAME_MAX` —
/// so 250 is tested (>246 of the harness, ≤255 of `MinIO`). Real AWS
/// accepts up to 1024 in the full key (ADR 0016 D); that total ceiling is
/// only validated by AWS, not `MinIO`.
#[tokio::test]
async fn keys_largas_byte_exactas() {
    let (_c, p, _op) = setup().await;
    let r = root();
    let long_name = "x".repeat(250);
    let f = child(&r, long_name.as_bytes());
    write_all(&p, &f, b"contenido").await;
    assert_eq!(read_all(&p, &f).await, b"contenido");
    // Appears byte-exact in the listing.
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
    assert!(listed.contains(&long_name.into_bytes()));
}

/// `copy_native` with a REAL `If-None-Match`: `MinIO` validates the
/// conditional copy (s3s-fs ignores it). Byte-exact copy; a second copy to
/// the same destination = Conflict.
#[tokio::test]
async fn copy_native_conditional_real() {
    let (_c, p, _op) = setup().await;
    let r = root();
    let src = child(&r, b"origen.bin");
    write_all(&p, &src, b"payload").await;
    let dst = child(&r, b"copia.bin");
    assert!(matches!(p.copy_native(&src, &dst).await, Some(Ok(()))));
    assert_eq!(read_all(&p, &dst).await, b"payload");
    // A second copy to the SAME destination → Conflict (If-None-Match).
    assert!(matches!(
        p.copy_native(&src, &dst).await,
        Some(Err(Error::Conflict { .. }))
    ));
}

/// Like [`setup`], but with the `Operator` rooted at a PREFIX
/// (`/team/project/`), which is what a real deployment sharing a bucket
/// does. It is the only way to exercise the key budget with `root`.
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
        .expect("start MinIO");
    let port = container.get_host_port_ipv4(9000).await.expect("port");
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

/// **The key budget deducts the `root` prefix, and the rejection happens
/// HERE, not at the server** (#50 point 2).
///
/// An S3 key is 1024 bytes counting the `root` the `Operator` prepends
/// before sending it. If the provider does not deduct it, it lets through
/// names the server rejects midway through an operation — and midway
/// through means with an open multipart and an error that does not say
/// what happened. The staircase is built around the EFFECTIVE budget, not
/// 1024: that is what distinguishes deducting the prefix from not
/// deducting it.
#[tokio::test]
async fn el_presupuesto_de_key_descuenta_el_prefijo_root() {
    let prefix = "equipo/proyecto/";
    let (_c, p, _op) = setup_con_root(&format!("/{prefix}")).await;
    let r = root();
    // 1024 − len(prefix) − 1 (the `/` the directory variant reserves).
    let budget = 1024 - prefix.len() - 1;

    // A name that fits EXACTLY. Not written: MinIO limits each component to
    // 255 bytes like a real NAME_MAX, so what this test exists to check —
    // where the boundary falls— is checked with a path of several short
    // segments.
    let segments = budget / 10; // "sssssssss/" = 10 bytes per round
    let mut fits = r.clone();
    for _ in 0..segments {
        fits = child(&fits, b"sssssssss");
    }
    // The last segment completes the exact budget.
    let remainder = budget - (segments * 10) + 1;
    if remainder > 0 {
        fits = child(&fits, "z".repeat(remainder).as_bytes());
    }
    // One byte more does NOT fit, and it is said BEFORE touching the network.
    let over = child(&fits, b"y");
    assert_eq!(
        p.stat(&over).await.unwrap_err(),
        Error::InvalidPath,
        "past the budget it has to be InvalidPath from here, not a 400 from the server"
    );

    // And the capabilities SAY so: `max_path` is the effective budget, not
    // 1024. A client that composes names needs the real number.
    let caps = p.capabilities_at(&r).await.expect("caps");
    assert_eq!(
        caps.max_path,
        Some(u32::try_from(budget).expect("fits")),
        "the capabilities announce the budget ALREADY deducted"
    );
}

/// **An echoed key SHORTER than `root` must not crash the task** (#50
/// point 4).
///
/// opendal 0.58's `build_rel_path` trims the key by `root`'s length with
/// only a `debug_assert`: in release, a key shorter than the prefix does
/// out-of-range slicing —or cuts mid multibyte character—. A server that
/// echoes something not starting with `root` (a liar, or a proxy that
/// rewrites) would turn that into a panic inside the Task.
///
/// Here it is seeded below the prefix with the RAW `Operator` —i.e. with
/// keys that DO carry the root— and what the provider promises is checked:
/// that listing and stating what was seeded from outside does not blow up.
/// Reproducing the panic requires a lying server, which is a different
/// piece; this pins that the normal path does not trigger it and leaves
/// the case written down.
#[tokio::test]
async fn una_siembra_bajo_el_root_no_revienta_el_listado() {
    let (_c, p, op) = setup_con_root("/equipo/proyecto/").await;
    let r = root();
    // The Operator already carries the root: this key is
    // `equipo/proyecto/desde-fuera/a.txt`.
    op.write("desde-fuera/a.txt", b"contenido".to_vec())
        .await
        .expect("seed");
    let dir = child(&r, b"desde-fuera");
    assert_eq!(p.stat(&dir).await.expect("stat").kind, EntryKind::Dir);
    assert_eq!(read_all(&p, &child(&dir, b"a.txt")).await, b"contenido");
    // And the root's listing sees it, with the prefix trim not taking away
    // any byte of the name.
    let names: Vec<Vec<u8>> = p
        .list(&r)
        .await
        .expect("list")
        .try_collect::<Vec<_>>()
        .await
        .expect("stream")
        .into_iter()
        .map(|e| e.path.file_name().expect("name").as_bytes().to_vec())
        .collect();
    assert!(names.contains(&b"desde-fuera".to_vec()), "{names:?}");
}

/// **Keys the provider CANNOT create but that CAN exist** (#50 point 3):
/// seeded by another tool, they have to give an honest answer —correct or
/// fail-loud— and never silent corruption.
///
/// The issue's three: a trailing space (which our `key()` rejects because
/// opendal would trim it, #48), an empty segment (`dir//x`) and an
/// empty-directory marker.
#[tokio::test]
async fn keys_sembradas_desde_fuera_que_nosotros_no_creariamos() {
    let (_c, p, op) = setup().await;
    let r = root();

    // 1. Trailing space. Our `key()` rejects it BEFORE the network because
    //    opendal's `normalize_path` does `trim()` and would corrupt the name.
    op.write("sembrado/con espacio ", b"a".to_vec())
        .await
        .expect("seed space");
    let with_space = child(&child(&r, b"sembrado"), b"con espacio ");
    assert_eq!(
        p.stat(&with_space).await.unwrap_err(),
        Error::InvalidPath,
        "a name opendal would trim is refused fail-loud, another file is not read instead"
    );

    // 2. Empty segment (`dir//x`): there is no `VPath` that can name it
    //    —`Segment` rejects the empty one— so the provider cannot request
    //    it even by accident. What matters is that its presence does not
    //    break the neighboring directory's listing.
    op.write("sembrado//hueco.txt", b"b".to_vec())
        .await
        .expect("seed empty");
    let listing = p.list(&child(&r, b"sembrado")).await;
    assert!(
        listing.is_ok(),
        "a key with an empty segment next to it must not crash the listing"
    );

    // 3. Empty-directory marker, which is how another tool represents a dir
    //    with no content. It has to show up as Dir.
    // `write`ing a key ending in `/` is refused by opendal itself
    // (`IsADirectory`), so the marker is seeded the way another tool would
    // seed it: with the create-directory operation.
    op.create_dir("vacio/").await.expect("seed marker");
    assert_eq!(
        p.stat(&child(&r, b"vacio"))
            .await
            .expect("stat marker")
            .kind,
        EntryKind::Dir
    );
}

/// The canonical corpus's LONG fixtures against a real S3 (#50 point 1).
///
/// The test above tries a hand-picked 250 bytes; these are the corpus's,
/// which is what the rest of the project uses to say "long name".
/// `name_over_max_256` goes past the 255 MinIO enforces per component: that
/// **is not a bug of ours** —AWS accepts up to 1024 in the full key— and
/// the test pins it for what it is, a difference between servers, instead
/// of leaving the claim unchecked.
#[tokio::test]
async fn las_fixtures_largas_del_corpus_contra_s3_real() {
    let (_c, p, _op) = setup().await;
    let r = root();
    let corpus = norte_testkit::corpus::hostile_names();

    for id in ["name_max_255", "name_max_255_multibyte"] {
        let f = corpus
            .iter()
            .find(|f| f.id == id)
            .unwrap_or_else(|| panic!("the canonical corpus has `{id}`"));
        let path = child(&r, &f.bytes);
        write_all(&p, &path, b"x").await;
        assert_eq!(read_all(&p, &path).await, b"x", "[{id}] round-trip");
        // And it comes back BYTE-EXACT from the listing: 255 multibyte
        // bytes is where a byte-count trim would split a character.
        let listing: Vec<Vec<u8>> = p
            .list(&r)
            .await
            .expect("list")
            .try_collect::<Vec<_>>()
            .await
            .expect("stream")
            .into_iter()
            .map(|e| e.path.file_name().expect("name").as_bytes().to_vec())
            .collect();
        assert!(
            listing.contains(&f.bytes),
            "[{id}] did not come back byte-exact"
        );
    }

    // 256 bytes: MinIO rejects it per component. The real verdict is
    // documented instead of assumed.
    let over = corpus
        .iter()
        .find(|f| f.id == "name_over_max_256")
        .expect("the corpus has `name_over_max_256`");
    let res = p.write(&child(&r, &over.bytes)).await;
    match res {
        Err(e) => {
            // What CANNOT happen is a panic or a silent success that later
            // cannot be read.
            eprintln!("name_over_max_256 against MinIO: {e:?}");
        }
        Ok(mut sink) => {
            let commit = async {
                sink.write(Bytes::from_static(b"x")).await?;
                sink.commit().await
            }
            .await;
            eprintln!("name_over_max_256 against MinIO: commit = {commit:?}");
        }
    }
}

/// REAL conditional write: two writes to the same key; the second commit
/// loses with Conflict (`If-None-Match` in the
/// `CompleteMultipartUpload`/`PutObject`).
#[tokio::test]
async fn conditional_write_real() {
    let (_c, p, _op) = setup().await;
    let f = child(&root(), b"unico.txt");
    write_all(&p, &f, b"primero").await;
    // A second write over the existing key: Conflict on open (stat-check) or
    // on commit (If-None-Match) — in both cases it never overwrites.
    match p.write(&f).await {
        Err(Error::Conflict { .. }) => {}
        Ok(mut sink) => {
            sink.write(Bytes::from_static(b"segundo"))
                .await
                .expect("chunk");
            assert!(matches!(sink.commit().await, Err(Error::Conflict { .. })));
        }
        Err(e) => panic!("expected Conflict, was {e:?}"),
    }
    assert_eq!(read_all(&p, &f).await, b"primero");
}
