//! `S3Connector` against an in-process S3 server (s3s-fs, the same pattern as
//! norte-vfs-object): the probe validates credentials/bucket, and the
//! resulting `Operator` operates. Linux-only (the harness relies on the
//! host's FS).
#![cfg(target_os = "linux")]

use std::net::SocketAddr;
use std::path::Path;

use norte_connect::{AuthMethod, ConnectionSpec, S3Connector, Secret};

const AK: &str = "norte-test-ak";
const SK: &str = "norte-test-sk";
const BUCKET: &str = "norte-test";

async fn start_s3s(root: &Path) -> SocketAddr {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto::Builder as ConnBuilder;
    use s3s::auth::SimpleAuth;
    use s3s::service::S3ServiceBuilder;

    std::fs::create_dir(root.join(BUCKET)).expect("bucket dir");
    let fs = s3s_fs::FileSystem::new(root).expect("s3s-fs");
    let service = {
        let mut b = S3ServiceBuilder::new(fs);
        b.set_auth(SimpleAuth::from_single(AK, SK));
        b.build()
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
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

/// access-key spec against the `addr` endpoint (path-style: there is an
/// endpoint).
fn spec_access_key(addr: SocketAddr) -> ConnectionSpec {
    ConnectionSpec {
        url: format!("s3://{BUCKET}"),
        auth: AuthMethod::AccessKey,
        key: None,
        tls: norte_connect::TlsMode::Require,
        region: Some("us-east-1".to_string()),
        endpoint: Some(format!("http://{addr}")),
        access_key_id: Some(AK.to_string()),
        addressing: Some(norte_connect::AddressingStyle::Path),
        logical_trash: false,
        allow_rsa: false,
        secret: norte_connect::SecretSource::Stored,
    }
}

#[tokio::test]
async fn connect_validates_and_operates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let addr = start_s3s(dir.path()).await;
    let secret = Secret::new(SK.to_string());
    let op = S3Connector::new()
        .connect(&spec_access_key(addr), Some(&secret))
        .await
        .expect("connect + probe OK");
    // The resulting Operator operates: write/read roundtrip.
    op.write("hello.txt", b"norte".to_vec())
        .await
        .expect("write");
    let back = op.read("hello.txt").await.expect("read").to_vec();
    assert_eq!(back, b"norte");
}

/// The probe does REAL I/O: a dead endpoint (nothing listening) fails at
/// `connect`, not on the first operation — the fail-fast the engine's
/// timeout expects. (Having a server REJECT bad credentials requires it to
/// validate sigv4 on the list, which s3s-fs does not do → nightly `MinIO`,
/// #50.)
#[tokio::test]
async fn connect_to_dead_endpoint_fails_at_the_probe() {
    // Closed ephemeral port: bind-then-drop guarantees nobody is listening.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let dead = probe.local_addr().expect("addr");
    drop(probe);
    let mut spec = spec_access_key(dead);
    spec.endpoint = Some(format!("http://{dead}"));
    let secret = Secret::new(SK.to_string());
    let err = S3Connector::new()
        .connect(&spec, Some(&secret))
        .await
        .expect_err("a dead endpoint must fail in the probe");
    let proto: norte_proto::Error = err.into();
    assert!(
        matches!(proto, norte_proto::Error::ProviderUnavailable { .. }),
        "was {proto:?}"
    );
}

#[tokio::test]
async fn access_key_sin_secret_es_error_local() {
    let dir = tempfile::tempdir().expect("tempdir");
    let addr = start_s3s(dir.path()).await;
    // No secret and auth=access-key: error BEFORE touching the network.
    let err = S3Connector::new()
        .connect(&spec_access_key(addr), None)
        .await
        .expect_err("access-key without secret");
    assert!(matches!(err, norte_connect::ConnectError::Secret { .. }));
}

/// #320 at the layer that HAD the bug: with `auth = "access-key"`, an empty
/// secret or an empty `access_key_id` must die here, before touching the
/// network.
///
/// The resolver also rejects them, but this is the only test that runs
/// against the connector — public API for which the resolver is not the
/// only possible caller. If someone tomorrow feeds credentials from
/// somewhere else (a flag, a plugin) and this guard is not there, the bug
/// comes back intact: opendal discards the empty string in both setters,
/// the static provider does not get registered and the connection
/// authenticates with whatever the environment offers.
#[tokio::test]
async fn empty_access_key_or_secret_is_local_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let addr = start_s3s(dir.path()).await;

    let empty = Secret::new(String::new());
    let err = S3Connector::new()
        .connect(&spec_access_key(addr), Some(&empty))
        .await
        .expect_err("an empty secret cannot connect");
    assert!(
        matches!(err, norte_connect::ConnectError::Secret { .. }),
        "was {err:?}"
    );

    let mut spec = spec_access_key(addr);
    spec.access_key_id = Some(String::new());
    let secret = Secret::new(SK.to_string());
    let err = S3Connector::new()
        .connect(&spec, Some(&secret))
        .await
        .expect_err("an empty access_key_id cannot connect");
    assert!(
        matches!(err, norte_connect::ConnectError::Config(_)),
        "was {err:?}"
    );
}

/// **#321: NO authentication method builds an s3 operator without explicit
/// credentials, except the one that asks for them on purpose.**
///
/// What makes `access-key` deterministic is not that the ambient chain is
/// off: `disable_config_load` only turns off env, profile and IMDS, and in
/// opendal 0.58 it leaves SSO, web-identity, process and ECS in place. What
/// makes it deterministic is that the static provider comes in first and
/// WINS — i.e. that there are always static credentials.
///
/// Hence this test, which does not check one case but a PROPERTY over the
/// whole enum: every variant either requires credentials, or is `Agent` —
/// the only one that asks for the chain on purpose, for the CI/instance-role
/// case— or is rejected. The risk this closes is the one the issue names:
/// that someone tomorrow adds a new `auth`, forgets to require credentials,
/// and the connection silently authenticates with whatever identity the
/// environment offers. An exhaustive `match` makes adding a variant fail to
/// compile until it is decided which group it belongs to.
///
/// Not tested with hostile environment variables because `std::env::set_var`
/// is `unsafe` in the 2024 edition and rule 5 forbids it; the property that
/// backs the guarantee is tested instead, which is the one that can break by
/// oversight.
#[tokio::test]
async fn no_auth_reaches_the_environment_chain_by_accident() {
    let dir = tempfile::tempdir().expect("tempdir");
    let addr = start_s3s(dir.path()).await;
    let secret = Secret::new(SK.to_string());

    for auth in [
        AuthMethod::Agent,
        AuthMethod::Key,
        AuthMethod::Password,
        AuthMethod::AccessKey,
    ] {
        let mut spec = spec_access_key(addr);
        spec.auth = auth;
        match auth {
            // The ONLY one that uses the ambient chain, and it does so
            // because it is asked to: instance role, CI, corporate
            // environment.
            AuthMethod::Agent => {}
            // Without static credentials nothing gets built: it is said.
            AuthMethod::Key | AuthMethod::Password => {
                let err = S3Connector::new()
                    .connect(&spec, Some(&secret))
                    .await
                    .expect_err("s3 does not accept key/password");
                assert!(
                    matches!(err, norte_connect::ConnectError::Config(_)),
                    "{auth:?}: {err:?}"
                );
            }
            // Requires both halves, and absent or empty is the same thing.
            AuthMethod::AccessKey => {
                let sin = S3Connector::new()
                    .connect(&spec, None)
                    .await
                    .expect_err("without a secret it does not connect");
                assert!(
                    matches!(sin, norte_connect::ConnectError::Secret { .. }),
                    "{sin:?}"
                );
            }
        }
    }
}

/// #320: an `endpoint`/`region` that is present and empty is not "unset" —
/// opendal discards them and the REAL destination becomes AWS (and the
/// region, the environment's). The user believes they are talking to their
/// `MinIO`. Rejected at construction time.
#[tokio::test]
async fn empty_endpoint_or_region_is_local_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let addr = start_s3s(dir.path()).await;
    let secret = Secret::new(SK.to_string());

    for which in [0, 1] {
        let mut spec = spec_access_key(addr);
        if which == 0 {
            spec.endpoint = Some(String::new());
        } else {
            spec.region = Some(String::new());
        }
        let err = S3Connector::new()
            .connect(&spec, Some(&secret))
            .await
            .expect_err("empty field");
        assert!(
            matches!(err, norte_connect::ConnectError::Config(_)),
            "was {err:?}"
        );
    }
}

/// Rule 10: the secret-access-key NEVER appears in the error (neither in the
/// `ConnectError` nor in its projection onto the protocol), even if the
/// probe fails. A net that catches a future regression in `map_opendal`
/// (e.g. to `e.to_string()`).
#[tokio::test]
async fn secret_access_key_never_in_the_error() {
    const SECRET: &str = "AKIA-SECRETO-QUE-NO-DEBE-FILTRARSE-42";
    // Dead endpoint → the probe fails at construction/network.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let dead = probe.local_addr().expect("addr");
    drop(probe);
    let mut spec = spec_access_key(dead);
    spec.endpoint = Some(format!("http://{dead}"));
    let secret = Secret::new(SECRET.to_string());
    let err = S3Connector::new()
        .connect(&spec, Some(&secret))
        .await
        .expect_err("dead endpoint");
    let connect_disp = format!("{err}");
    let proto: norte_proto::Error = err.into();
    let proto_disp = format!("{proto:?}");
    assert!(
        !connect_disp.contains(SECRET) && !proto_disp.contains(SECRET),
        "the secret leaked: connect={connect_disp:?} proto={proto_disp:?}"
    );
}

#[tokio::test]
async fn region_ausente_sin_endpoint_es_error() {
    let spec = ConnectionSpec {
        url: format!("s3://{BUCKET}"),
        auth: AuthMethod::AccessKey,
        key: None,
        tls: norte_connect::TlsMode::Require,
        region: None,
        endpoint: None, // AWS
        access_key_id: Some(AK.to_string()),
        addressing: None,
        logical_trash: false,
        allow_rsa: false,
        secret: norte_connect::SecretSource::Stored,
    };
    let secret = Secret::new(SK.to_string());
    let err = S3Connector::new()
        .connect(&spec, Some(&secret))
        .await
        .expect_err("AWS without region");
    assert!(matches!(err, norte_connect::ConnectError::Config(_)));
}
