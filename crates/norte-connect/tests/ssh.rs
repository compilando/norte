//! `SshConnector` integration against an **in-process** SSH server (russh
//! server, no Docker): full TOFU cycle (unknown → trust → known →
//! mismatch), auth by password / ed25519 key / agent, RSA rejection
//! (ADR 0015 D/E). The nightly job with real OpenSSH (`norte-vfs-sftp/openssh.rs`)
//! covers interop with a production server; this covers the LOGIC.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use norte_connect::{
    AuthMethod, ConnectError, ConnectionSpec, KnownHostsStore, Secret, SshConnector, TlsMode,
};
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, HashAlg, PrivateKey, PublicKey};
use russh::server::{Auth, Msg, Session};
use russh::{Channel, ChannelId};

const USER: &str = "norte";
const PASS: &str = "s3cr3t";

/// Ephemeral test ed25519 key.
fn key() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("generate ed25519")
}

fn fingerprint(pk: &PublicKey) -> String {
    pk.fingerprint(HashAlg::Sha256).to_string()
}

/// Test server handler: fixed password, an optional authorized pubkey and a
/// trivial sftp subsystem (init/version handshake only).
struct ServerHandler {
    allowed_pubkey: Option<PublicKey>,
    channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
}

impl russh::server::Handler for ServerHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == USER && password == PASS {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        // By `key_data` and not by `PublicKey`: its `==` includes the
        // COMMENT, which the RSA fixture carries and the key received over
        // the wire does not.
        let authorized = self
            .allowed_pubkey
            .as_ref()
            .is_some_and(|k| k.key_data() == public_key.key_data());
        Ok(if user == USER && authorized {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels
            .lock()
            .expect("test lock")
            .insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp" {
            let channel = self
                .channels
                .lock()
                .expect("test lock")
                .remove(&channel_id)
                .expect("channel open before the subsystem");
            session.channel_success(channel_id)?;
            tokio::spawn(russh_sftp::server::run(channel.into_stream(), TrivialSftp));
        } else {
            session.channel_failure(channel_id)?;
        }
        Ok(())
    }
}

/// Minimal sftp server: the framework answers init/version; everything else
/// is `OpUnsupported` (enough to test `SftpSession`'s handshake).
struct TrivialSftp;

impl russh_sftp::server::Handler for TrivialSftp {
    type Error = russh_sftp::protocol::StatusCode;

    fn unimplemented(&self) -> Self::Error {
        russh_sftp::protocol::StatusCode::OpUnsupported
    }
}

/// Starts the test SSH server on 127.0.0.1:0 and returns the port.
async fn spawn_server(host_key: PrivateKey, allowed_pubkey: Option<PublicKey>) -> u16 {
    spawn_server_with(host_key, allowed_pubkey, russh::Preferred::default()).await
}

/// Like [`spawn_server`], with custom preferred algorithms: its `key` list is
/// also what the server announces in `server-sig-algs`.
async fn spawn_server_with(
    host_key: PrivateKey,
    allowed_pubkey: Option<PublicKey>,
    preferred: russh::Preferred,
) -> u16 {
    let config = Arc::new(russh::server::Config {
        keys: vec![host_key],
        auth_rejection_time: std::time::Duration::from_millis(10),
        auth_rejection_time_initial: Some(std::time::Duration::ZERO),
        preferred,
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("test server bind");
    let port = listener.local_addr().expect("local addr").port();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let handler = ServerHandler {
                allowed_pubkey: allowed_pubkey.clone(),
                channels: Arc::new(Mutex::new(HashMap::new())),
            };
            let config = Arc::clone(&config);
            tokio::spawn(async move {
                if let Ok(session) = russh::server::run_stream(config, stream, handler).await {
                    let _ = session.await;
                }
            });
        }
    });
    port
}

/// Connector with an empty store in a tempdir (virgin TOFU) and no agent.
fn connector(dir: &Path) -> SshConnector {
    SshConnector {
        known_hosts: KnownHostsStore::at(dir.join("known_hosts")),
        agent_socket: None,
    }
}

fn spec_password(port: u16) -> ConnectionSpec {
    ConnectionSpec {
        url: format!("sftp://{USER}@127.0.0.1:{port}"),
        auth: AuthMethod::Password,
        key: None,
        tls: TlsMode::Require,
        ..Default::default()
    }
}

/// Like `unwrap_err`, but `SftpSession` does not implement `Debug`.
async fn connect_err(
    conn: &SshConnector,
    spec: &ConnectionSpec,
    secret: Option<&Secret>,
) -> ConnectError {
    match conn.connect(spec, secret).await {
        Ok(_) => panic!("expected a connect error"),
        Err(e) => e,
    }
}

/// Full TOFU cycle: first contact → `HostKeyUnknown` with a fingerprint,
/// explicit trust → real connection and sftp handshake.
#[tokio::test]
async fn unknown_tofu_trust_and_connect() {
    let host_key = key();
    let fp_real = fingerprint(host_key.public_key());
    let port = spawn_server(host_key, None).await;
    let dir = tempfile::tempdir().unwrap();
    let conn = connector(dir.path());
    let spec = spec_password(port);
    let secret = Secret::new(PASS.into());

    // 1. First contact: NEVER connects blindly.
    let err = connect_err(&conn, &spec, Some(&secret)).await;
    let ConnectError::HostKeyUnknown {
        host,
        port: p,
        algo,
        fingerprint: fp,
    } = err
    else {
        panic!("expected HostKeyUnknown, was {err:?}");
    };
    assert_eq!(host, "127.0.0.1");
    assert_eq!(p, port);
    assert_eq!(algo, "ssh-ed25519");
    assert_eq!(fp, fp_real);

    // 2. Explicit confirmation (the connection.trust_host_key flow).
    conn.trust_host_key(&host, p, &fp).await.unwrap();

    // 3. Retry: the key is now trusted; auth + sftp handshake OK.
    let _session = conn.connect(&spec, Some(&secret)).await.unwrap();
}

/// Trust re-verifies the fingerprint against the host's REAL key
/// (anti-TOCTOU): a fingerprint that does not match registers nothing.
#[tokio::test]
async fn trust_with_wrong_fingerprint_does_not_register() {
    let host_key = key();
    let port = spawn_server(host_key, None).await;
    let dir = tempfile::tempdir().unwrap();
    let conn = connector(dir.path());
    let fake_fp = fingerprint(key().public_key());

    let err = conn
        .trust_host_key("127.0.0.1", port, &fake_fp)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ConnectError::HostKeyMismatch { .. }),
        "was {err:?}"
    );

    // Still not registered: the next connect is a first contact.
    let err = connect_err(&conn, &spec_password(port), Some(&Secret::new(PASS.into()))).await;
    assert!(
        matches!(err, ConnectError::HostKeyUnknown { .. }),
        "was {err:?}"
    );
}

/// A REGISTERED host key that changes is a `HostKeyMismatch` (possible
/// MITM): never connects nor re-registers silently.
#[tokio::test]
async fn host_key_cambiada_es_mismatch() {
    let host_key = key();
    let fp_presented = fingerprint(host_key.public_key());
    let port = spawn_server(host_key, None).await;
    let dir = tempfile::tempdir().unwrap();
    // The store already has ANOTHER key registered for this host:port
    // (OpenSSH format: it can be pre-populated by hand, ADR 0015 D).
    let registered = key().public_key().to_openssh().unwrap();
    std::fs::write(
        dir.path().join("known_hosts"),
        format!("[127.0.0.1]:{port} {registered}\n"),
    )
    .unwrap();
    let conn = connector(dir.path());

    let err = connect_err(&conn, &spec_password(port), Some(&Secret::new(PASS.into()))).await;
    let ConnectError::HostKeyMismatch {
        fingerprint: fp, ..
    } = err
    else {
        panic!("expected HostKeyMismatch, was {err:?}");
    };
    // The reported fingerprint is the PRESENTED (suspicious) key's.
    assert_eq!(fp, fp_presented);
}

/// Wrong password → `AuthFailed` (no secret material in the error).
#[tokio::test]
async fn password_incorrecta_es_auth_failed() {
    let host_key = key();
    let fp = fingerprint(host_key.public_key());
    let port = spawn_server(host_key, None).await;
    let dir = tempfile::tempdir().unwrap();
    let conn = connector(dir.path());
    conn.trust_host_key("127.0.0.1", port, &fp).await.unwrap();

    let err = connect_err(
        &conn,
        &spec_password(port),
        Some(&Secret::new("wrong".into())),
    )
    .await;
    let ConnectError::AuthFailed { user, host } = &err else {
        panic!("expected AuthFailed, was {err:?}");
    };
    assert_eq!(user, USER);
    assert_eq!(host, "127.0.0.1");
    assert!(
        !format!("{err}").contains("wrong"),
        "the error must not carry the password"
    );
}

/// Auth by an ed25519 key ENCRYPTED with a passphrase: the passphrase
/// arrives as a `Secret` (resolved by the `SecretResolver` in the real
/// flow).
#[tokio::test]
async fn auth_by_encrypted_ed25519_key() {
    let host_key = key();
    let fp = fingerprint(host_key.public_key());
    let client_key = key();
    let port = spawn_server(host_key, Some(client_key.public_key().clone())).await;
    let dir = tempfile::tempdir().unwrap();
    let conn = connector(dir.path());
    conn.trust_host_key("127.0.0.1", port, &fp).await.unwrap();

    let encrypted = client_key
        .encrypt(&mut rand::rng(), "test-passphrase")
        .expect("encrypt test key");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(&key_path, encrypted.to_openssh(LineEnding::LF).unwrap()).unwrap();

    let spec = ConnectionSpec {
        url: format!("sftp://{USER}@127.0.0.1:{port}"),
        auth: AuthMethod::Key,
        key: Some(key_path.clone()),
        tls: TlsMode::Require,
        ..Default::default()
    };
    let _session = conn
        .connect(&spec, Some(&Secret::new("test-passphrase".into())))
        .await
        .unwrap();

    // Wrong passphrase → clear KeyLoad (and no passphrase in the message).
    let err = connect_err(&conn, &spec, Some(&Secret::new("otra".into()))).await;
    assert!(matches!(err, ConnectError::KeyLoad { .. }), "was {err:?}");
    assert!(!format!("{err}").contains("otra"));
}

/// An RSA key is rejected BEFORE touching the network, with an error that
/// recommends ed25519 (ADR 0015 E, closes #36 / RUSTSEC-2023-0071).
#[tokio::test]
async fn rsa_key_rejected_without_network() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("id_rsa");
    std::fs::write(&key_path, RSA_FIXTURE).unwrap();
    let conn = connector(dir.path());

    let spec = ConnectionSpec {
        // Port 1: closed — if connect touched the network before validating
        // the key, the error would be a connection one, not KeyUnsupported.
        url: format!("sftp://{USER}@127.0.0.1:1"),
        auth: AuthMethod::Key,
        key: Some(key_path),
        tls: TlsMode::Require,
        ..Default::default()
    };
    let err = connect_err(&conn, &spec, None).await;
    let ConnectError::KeyUnsupported { algo, .. } = &err else {
        panic!("expected KeyUnsupported, was {err:?}");
    };
    assert!(algo.contains("rsa"), "algo was {algo}");
    assert!(
        format!("{err}").contains("ed25519"),
        "the error must recommend ed25519"
    );
}

/// RSA fixture generated for tests (never used for anything real).
const RSA_FIXTURE: &str = include_str!("fixtures/id_rsa_test");

/// `auth = "key"` spec over the RSA fixture, with `allow_rsa` to choose.
fn spec_rsa(dir: &Path, port: u16, allow_rsa: bool) -> ConnectionSpec {
    let key_path = dir.join("id_rsa");
    std::fs::write(&key_path, RSA_FIXTURE).unwrap();
    ConnectionSpec {
        url: format!("sftp://{USER}@127.0.0.1:{port}"),
        auth: AuthMethod::Key,
        key: Some(key_path),
        tls: TlsMode::Require,
        allow_rsa,
        ..Default::default()
    }
}

/// With `allow_rsa = true` (ADR 0150) the SAME RSA key that is rejected by
/// default authenticates, signing with rsa-sha2 negotiated via
/// `server-sig-algs`.
#[tokio::test]
async fn rsa_key_with_allow_rsa_authenticates() {
    let host_key = key();
    let fp = fingerprint(host_key.public_key());
    let rsa = PrivateKey::from_openssh(RSA_FIXTURE).expect("RSA fixture");
    let port = spawn_server(host_key, Some(rsa.public_key().clone())).await;
    let dir = tempfile::tempdir().unwrap();
    let conn = connector(dir.path());
    conn.trust_host_key("127.0.0.1", port, &fp).await.unwrap();

    let _session = conn
        .connect(&spec_rsa(dir.path(), port, true), None)
        .await
        .unwrap();
}

/// **1024-bit** RSA fixture, generated for this test and nothing else.
///
/// It is in the repository on purpose, just like the 2048-bit one:
/// generating a key on every debug test run would cost more than the test,
/// and what is being tested is not the generator.
const RSA_1024_FIXTURE: &str = include_str!("fixtures/id_rsa_1024_test");

/// **`allow_rsa` does not open up ANY RSA** (#370).
///
/// A 1024-bit key is rejected even if the connection has signed the opt-in,
/// and with its own error: the fix is not "turn on RSA" —it already is—
/// but getting another key, and `KeyUnsupported` would say the opposite.
///
/// Why the opt-in does not lift it: ADR 0150 buys ONE named risk —the
/// RUSTSEC-2023-0071 timing side channel— identical at 1024 and at 4096
/// bits. A short modulus is a different risk, the ADR does not mention it,
/// and whoever signed off on the opt-in was carrying it without anyone
/// telling them.
#[tokio::test]
async fn a_1024_bit_rsa_key_is_rejected_even_with_allow_rsa_set() {
    let host_key = key();
    let fp = fingerprint(host_key.public_key());
    let rsa = PrivateKey::from_openssh(RSA_1024_FIXTURE).expect("1024-bit RSA fixture");
    let port = spawn_server(host_key, Some(rsa.public_key().clone())).await;
    let dir = tempfile::tempdir().unwrap();
    let conn = connector(dir.path());
    conn.trust_host_key("127.0.0.1", port, &fp).await.unwrap();

    let key_path = dir.path().join("id_rsa_1024");
    std::fs::write(&key_path, RSA_1024_FIXTURE).unwrap();
    let spec = ConnectionSpec {
        url: format!("sftp://{USER}@127.0.0.1:{port}"),
        auth: AuthMethod::Key,
        key: Some(key_path),
        tls: TlsMode::Require,
        allow_rsa: true,
        ..Default::default()
    };

    let err = connect_err(&conn, &spec, None).await;
    match err {
        ConnectError::RsaTooSmall { bits, min, .. } => {
            assert_eq!(bits, 1024, "says how many it has");
            assert_eq!(min, 2048, "and how many are needed");
        }
        other => panic!("should have said the modulus is short, was {other:?}"),
    }
}

/// A server that only accepts `ssh-rsa` (SHA-1 signature) is rejected with
/// its own error: the opt-in opens up RSA, NEVER SHA-1.
#[tokio::test]
async fn rsa_key_against_sha1_only_server_is_an_error() {
    let host_key = key();
    let fp = fingerprint(host_key.public_key());
    let rsa = PrivateKey::from_openssh(RSA_FIXTURE).expect("RSA fixture");
    let preferred = russh::Preferred {
        key: std::borrow::Cow::Owned(vec![Algorithm::Ed25519, Algorithm::Rsa { hash: None }]),
        ..Default::default()
    };
    let port = spawn_server_with(host_key, Some(rsa.public_key().clone()), preferred).await;
    let dir = tempfile::tempdir().unwrap();
    let conn = connector(dir.path());
    conn.trust_host_key("127.0.0.1", port, &fp).await.unwrap();

    let err = connect_err(&conn, &spec_rsa(dir.path(), port, true), None).await;
    assert!(
        matches!(err, ConnectError::RsaSha1Only { .. }),
        "was {err:?}"
    );
}

/// Auth via SSH agent (in-process, same protocol as a real ssh-agent).
#[tokio::test]
async fn auth_by_agent() {
    let host_key = key();
    let fp = fingerprint(host_key.public_key());
    let client_key = key();
    let port = spawn_server(host_key, Some(client_key.public_key().clone())).await;
    let dir = tempfile::tempdir().unwrap();

    // In-process agent over a unix socket in the tempdir.
    let sock = dir.path().join("agent.sock");
    let listener = tokio::net::UnixListener::bind(&sock).expect("agent bind");
    tokio::spawn(russh::keys::agent::server::serve(
        tokio_stream::wrappers::UnixListenerStream::new(listener),
        (),
    ));
    let mut agent = russh::keys::agent::client::AgentClient::connect_uds(&sock)
        .await
        .expect("connect to the agent");
    agent
        .add_identity(&client_key, &[])
        .await
        .expect("add identity");
    drop(agent);

    let conn = SshConnector {
        known_hosts: KnownHostsStore::at(dir.path().join("known_hosts")),
        agent_socket: Some(sock),
    };
    conn.trust_host_key("127.0.0.1", port, &fp).await.unwrap();

    let spec = ConnectionSpec {
        url: format!("sftp://{USER}@127.0.0.1:{port}"),
        auth: AuthMethod::Agent,
        key: None,
        tls: TlsMode::Require,
        ..Default::default()
    };
    let _session = conn.connect(&spec, None).await.unwrap();
}

/// Trust over a host that ALREADY has ANOTHER key registered:
/// `HostKeyMismatch` (the wire's rotation/MITM category) and the store stays
/// intact. (russh's `learn` only appends: "patching on top" would leave the
/// file with two conflicting keys and the check stuck in a perpetual
/// Mismatch despite a trust that reported success.)
#[tokio::test]
async fn trust_over_a_different_registered_key_is_an_error() {
    let host_key = key();
    let fp_new = fingerprint(host_key.public_key());
    let port = spawn_server(host_key, None).await;
    let dir = tempfile::tempdir().unwrap();
    let old = key().public_key().to_openssh().unwrap();
    let old_line = format!("[127.0.0.1]:{port} {old}\n");
    std::fs::write(dir.path().join("known_hosts"), &old_line).unwrap();
    let conn = connector(dir.path());

    let err = conn
        .trust_host_key("127.0.0.1", port, &fp_new)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ConnectError::HostKeyMismatch { .. }),
        "was {err:?}"
    );
    // The file did NOT change: only the old entry remains.
    let content = std::fs::read_to_string(dir.path().join("known_hosts")).unwrap();
    assert_eq!(content, old_line);
}

/// With no agent available, `auth = "agent"` is a clear error, not a hang.
#[tokio::test]
async fn missing_agent_is_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let conn = connector(dir.path()); // agent_socket: None
    let spec = ConnectionSpec {
        url: format!("sftp://{USER}@127.0.0.1:1"),
        auth: AuthMethod::Agent,
        key: None,
        tls: TlsMode::Require,
        ..Default::default()
    };
    let err = connect_err(&conn, &spec, None).await;
    assert!(matches!(err, ConnectError::Agent(_)), "was {err:?}");
}

/// The URL must be sftp:// — an ftp scheme here is a usage error, not a
/// hang.
#[tokio::test]
async fn scheme_no_sftp_es_error() {
    let dir = tempfile::tempdir().unwrap();
    let conn = connector(dir.path());
    let spec = ConnectionSpec {
        url: "ftp://u@h:21".into(),
        auth: AuthMethod::Password,
        key: None,
        tls: TlsMode::Require,
        ..Default::default()
    };
    let err = connect_err(&conn, &spec, None).await;
    assert!(matches!(err, ConnectError::InvalidUrl(_)), "was {err:?}");
}
