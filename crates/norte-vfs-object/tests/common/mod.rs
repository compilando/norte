//! The object provider's test harnesses (ADR 0016 J). TWO backends:
//!
//! - [`fs_operator`]: opendal's `services-fs` over a tempdir — runs the
//!   FULL CONTRACT (the same provider logic, no HTTP). The 7b spike
//!   measured that s3s-fs is unfaithful exactly where the contract gets
//!   tight: it loses empty-dir markers when listing and blows up (500) with
//!   names close to the underlying fs's `NAME_MAX`.
//! - [`s3_operator`]: an IN-PROCESS S3 server (`s3s` + `s3s-fs` on
//!   `127.0.0.1:ephemeral-port`, the libunftp pattern) — runs the
//!   S3-SPECIFIC suite (tests/s3.rs): multipart, pre-commit invisibility,
//!   conditional write, ranges, names. There the spike measured it as
//!   FAITHFUL. s3s-fs's measured unfaithful spots (do NOT use it for
//!   these): empty-dir markers invisible to `LIST`, names close to
//!   `NAME_MAX` → 500, `HeadObject` of a directory path → 500 (real S3:
//!   404) and `copy_with` `if_not_exists` ignored. Anything touching dirs
//!   or long keys → nightly (`MinIO`).
//!
//! The nightly job (feature `it-s3`, tests/reals3.rs) validates against
//! real `MinIO`.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::Path;

use bytes::Bytes;
use norte_proto::{Error, VPath};
use norte_vfs::Provider;
use norte_vfs_object::ObjectProvider;
use opendal::Operator;

/// Toy credentials for the in-process server (not a secret).
pub const TEST_AK: &str = "norte-test-ak";
pub const TEST_SK: &str = "norte-test-sk";
pub const TEST_BUCKET: &str = "norte-test";

/// Writes `data` to `f` (write + commit), shared by the suites.
pub async fn write_all(p: &ObjectProvider, f: &VPath, data: &[u8]) {
    let mut sink = p.write(f).await.expect("write");
    sink.write(Bytes::copy_from_slice(data))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

/// Reads all of `f` into bytes.
///
/// # Errors
/// Propagates the provider's error (useful for tests expecting `NotFound`).
pub async fn read_all(p: &ObjectProvider, f: &VPath) -> Result<Vec<u8>, Error> {
    use futures::TryStreamExt as _;
    let mut s = p.read(f, None).await?;
    let mut out = Vec::new();
    while let Some(chunk) = s.try_next().await? {
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Installs opendal's default HTTP transport (idempotent). With
/// `default-features = false` opendal does NOT auto-register it.
pub fn install_opendal() {
    opendal::install_default();
}

/// `services-fs` `Operator` rooted at `base` (tempdir), with
/// `atomic_write_dir` — without it, fs's writer writes DIRECTLY to the
/// final path and `write_invisible_before_commit` would fail (on S3
/// invisibility is given by the multipart; here it is given by
/// tempfile+rename).
pub fn fs_operator(base: &Path, atomic: &Path) -> Operator {
    install_opendal();
    let b = opendal::services::Fs::default()
        .root(base.to_str().expect("UTF-8 tempdir"))
        .atomic_write_dir(atomic.to_str().expect("UTF-8 tempdir"));
    Operator::new(b).expect("fs operator")
}

/// Starts an in-process S3 server (`s3s-fs` over `root`) on an ephemeral
/// port and returns its address. [`TEST_BUCKET`] is created.
pub async fn start_s3s(root: &Path) -> SocketAddr {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto::Builder as ConnBuilder;
    use s3s::auth::SimpleAuth;
    use s3s::service::S3ServiceBuilder;

    std::fs::create_dir(root.join(TEST_BUCKET)).expect("bucket dir");
    let fs = s3s_fs::FileSystem::new(root).expect("s3s-fs");
    let service = {
        let mut b = S3ServiceBuilder::new(fs);
        b.set_auth(SimpleAuth::from_single(TEST_AK, TEST_SK));
        b.build()
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("ephemeral bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let service = service.clone();
            tokio::spawn(async move {
                let http = ConnBuilder::new(TokioExecutor::new());
                let _ = http.serve_connection(TokioIo::new(socket), service).await;
            });
        }
    });
    addr
}

/// S3 `Operator` (path-style, toy credentials) against [`start_s3s`].
pub fn s3_operator(addr: SocketAddr) -> Operator {
    install_opendal();
    let b = opendal::services::S3::default()
        .bucket(TEST_BUCKET)
        .region("us-east-1")
        .endpoint(&format!("http://{addr}"))
        .access_key_id(TEST_AK)
        .secret_access_key(TEST_SK)
        .disable_config_load()
        .disable_ec2_metadata();
    Operator::new(b).expect("s3 operator")
}
