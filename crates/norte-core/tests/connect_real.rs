//! Integration of the REAL [`ConnectionManager`] against in-process servers
//! (phase 6e): sftp (russh server) and ftp (libunftp), with `connections.toml`
//! and secrets from `secrets.age` in a tempdir — the full path
//! resolution → secret → transport → provider, with no Docker.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use norte_connect::{Secret, SecretResolver};
use norte_core::connect::{ConnectionManager, ConnectionWarningReason, RemoteConnector, named_url};
use norte_proto::Error;
use russh::server::{Auth, Msg, Session};
use russh::{Channel, ChannelId};

const USER: &str = "norte";
const PASS: &str = "s3cr3t";

// ---------- in-process SSH server (password + trivial sftp) ----------

struct ServerHandler {
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
                .expect("open channel");
            session.channel_success(channel_id)?;
            tokio::spawn(russh_sftp::server::run(channel.into_stream(), TrivialSftp));
        } else {
            session.channel_failure(channel_id)?;
        }
        Ok(())
    }
}

struct TrivialSftp;

impl russh_sftp::server::Handler for TrivialSftp {
    type Error = russh_sftp::protocol::StatusCode;

    fn unimplemented(&self) -> Self::Error {
        russh_sftp::protocol::StatusCode::OpUnsupported
    }
}

/// Starts the SSH server and returns (port, its host key's fingerprint).
async fn spawn_ssh() -> (u16, String) {
    let host_key =
        russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)
            .expect("test host key");
    let fingerprint = host_key
        .public_key()
        .fingerprint(russh::keys::HashAlg::Sha256)
        .to_string();
    let config = Arc::new(russh::server::Config {
        keys: vec![host_key],
        auth_rejection_time: std::time::Duration::from_millis(10),
        auth_rejection_time_initial: Some(std::time::Duration::ZERO),
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let handler = ServerHandler {
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
    (port, fingerprint)
}

// ---------- test config (connections.toml + secrets.age) ----------

/// Populates the config dir: the connection named `name` and its secret in
/// `secrets.age` (this exercises the real resolver, without touching global
/// env).
async fn config_with(dir: &Path, name: &str, url: &str, auth: &str) {
    std::fs::write(
        dir.join("connections.toml"),
        format!("[connections.{name}]\nurl = \"{url}\"\nauth = \"{auth}\"\n"),
    )
    .unwrap();
    let key_path = dir.join("secrets.key");
    std::fs::write(&key_path, "test-passphrase").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    SecretResolver::new(dir)
        .store_in_age(name, &Secret::new(PASS.into()))
        .await
        .expect("store the test secret");
}

// ---------- tests ----------

/// Full sftp path: first contact → `HostKeyUnknown` (the REAL key's
/// fingerprint), trust via the manager (re-verifies), connect → cacheable live
/// provider. Resolution matches the named entry and pulls the secret from
/// `secrets.age`.
#[tokio::test]
async fn sftp_tofu_trust_and_provider() {
    let (port, fp_real) = spawn_ssh().await;
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sftp://{USER}@127.0.0.1:{port}");
    config_with(dir.path(), "work", &url, "password").await;
    let mgr = ConnectionManager::new(dir.path());

    // First contact.
    let err = mgr
        .connect("sftp", &format!("{USER}@127.0.0.1:{port}"))
        .await
        .err()
        .map(|d| d.error)
        .expect("first contact must fail");
    let Error::HostKeyUnknown {
        host,
        port: p,
        fingerprint,
        ..
    } = &err
    else {
        panic!("expected HostKeyUnknown, was {err:?}");
    };
    assert_eq!(host, "127.0.0.1");
    assert_eq!(*p, Some(port));
    assert_eq!(fingerprint, &fp_real);

    // Explicit trust (anti-TOCTOU: it redials and compares) and a retry.
    mgr.trust_host_key(host, *p, fingerprint)
        .await
        .expect("trust");
    let connected = mgr
        .connect("sftp", &format!("{USER}@127.0.0.1:{port}"))
        .await
        .expect("connect after trust");
    assert_eq!(connected.provider.scheme(), "sftp");
    assert!(connected.warnings.is_empty(), "sftp does not degrade TLS");

    // connect_named uses the same entry (and the key is already trusted).
    let (scheme, authority, _prov) = mgr.connect_named("work").await.expect("connect_named");
    assert_eq!(scheme, "sftp");
    assert_eq!(authority, format!("{USER}@127.0.0.1:{port}"));

    // named_url resolves the entry's URL; a name that does not exist is
    // NotFound.
    assert_eq!(named_url(dir.path(), "work").await.unwrap(), url);
    assert!(matches!(
        named_url(dir.path(), "does-not-exist").await,
        Err(Error::NotFound)
    ));
}

/// A fingerprint that does NOT match the real key registers nothing.
#[tokio::test]
async fn trust_with_a_fake_fingerprint_is_a_mismatch() {
    let (port, _fp) = spawn_ssh().await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = ConnectionManager::new(dir.path());
    let err = mgr
        .trust_host_key(
            "127.0.0.1",
            Some(port),
            "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::HostKeyMismatch { .. }), "was {err:?}");
}

/// ftp path (#30 stage 3c, ADR 0033): `ftp://` goes through the WASM
/// provider-plugin. The manager resolves the IP, instantiates the embedded
/// guest and configures it against the server; the resulting provider serves
/// the `ftp` scheme and ALWAYS warns `FtpPlaintext` (FTPS = debt, the session
/// is plaintext).
#[tokio::test]
async fn ftp_establishes_a_provider_via_plugin() {
    use libunftp::ServerBuilder;
    use unftp_sbe_fs::Filesystem;

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral bind");
    let port = probe.local_addr().expect("addr").port();
    drop(probe);
    let server = ServerBuilder::new(Box::new(move || {
        Filesystem::new(home.clone()).expect("fs backend")
    }))
    .greeting("norte core test")
    .build()
    .expect("build");
    tokio::spawn(async move {
        let _ = server.listen(format!("127.0.0.1:{port}")).await;
    });
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let cfg = tempfile::tempdir().unwrap();
    std::fs::write(
        cfg.path().join("connections.toml"),
        format!(
            "[connections.backup]\nurl = \"ftp://anonymous@127.0.0.1:{port}\"\nauth = \"agent\"\ntls = \"plain\"\n"
        ),
    )
    .unwrap();
    let mgr = ConnectionManager::new(cfg.path());
    let connected = mgr
        .connect("ftp", &format!("anonymous@127.0.0.1:{port}"))
        .await
        .expect("connect ftp");
    assert_eq!(norte_vfs::Provider::scheme(&*connected.provider), "ftp");
    // FTP-via-plugin is ALWAYS plaintext (FTPS = debt): an FtpPlaintext warning.
    assert!(
        connected
            .warnings
            .iter()
            .any(|w| w.reason == ConnectionWarningReason::FtpPlaintext),
        "ftp-via-plugin warns FtpPlaintext, was {:?}",
        connected.warnings
    );
    // And the provider really LISTS the root (the guest configured and connects).
    let root = norte_proto::VPath::root(norte_proto::Scheme::new("ftp").unwrap(), None);
    let _stream = norte_vfs::Provider::list(&*connected.provider, &root)
        .await
        .expect("lists the remote root");
}

/// A scheme the manager does not know how to connect is Unsupported.
#[tokio::test]
async fn unknown_scheme_is_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ConnectionManager::new(dir.path());
    let err = mgr
        .connect("gopher", "h")
        .await
        .err()
        .map(|d| d.error)
        .expect("gopher does not connect");
    // The URL gopher://h does not even parse as a known remote connection.
    assert!(
        matches!(err, Error::Unsupported | Error::InvalidPath),
        "was {err:?}"
    );
}
