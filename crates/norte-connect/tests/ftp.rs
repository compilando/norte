//! `FtpConnector` integration against libunftp **in-process** (no Docker):
//! ADR 0015 F / issue #38's TLS policy — `require` demands FTPS and
//! validates the cert, `allow` degrades to plain WITH a warning, `plain` is
//! an explicit opt-in — plus anonymous/password login and rejecting
//! `auth = "key"`.

use std::path::Path;
use std::time::Duration;

use libunftp::ServerBuilder;
use norte_connect::{AuthMethod, ConnectError, ConnectionSpec, FtpConnector, Secret, TlsMode};
use unftp_sbe_fs::Filesystem;

/// Ephemeral self-signed cert for 127.0.0.1 → (cert.pem, key.pem) in `dir`.
fn cert_de_test(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()])
        .expect("generate test cert");
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    std::fs::write(&cert_path, cert.cert.pem()).unwrap();
    std::fs::write(&key_path, cert.signing_key.serialize_pem()).unwrap();
    (cert_path, key_path)
}

/// Starts libunftp in-process (anonymous, tempdir backend) on an ephemeral
/// port. With `ftps = Some((cert, key))` it requires AUTH TLS before login.
async fn server(base: &Path, ftps: Option<(std::path::PathBuf, std::path::PathBuf)>) -> u16 {
    let home = base.to_path_buf();
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral bind");
    let port = probe.local_addr().expect("addr").port();
    drop(probe);

    let mut builder = ServerBuilder::new(Box::new(move || {
        Filesystem::new(home.clone()).expect("fs backend")
    }))
    .greeting("norte test ftps");
    if let Some((cert, key)) = ftps {
        builder = builder.ftps(cert, key);
    }
    let server = builder.build().expect("build server");
    tokio::spawn(async move {
        // Panic with a cause: if another process stole the ephemeral port in
        // the bind-then-drop window, this is the real diagnosis, not the
        // timeout.
        if let Err(e) = server.listen(format!("127.0.0.1:{port}")).await {
            panic!("test ftp listen: {e}");
        }
    });
    // Busy-wait until the listener is alive.
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return port;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the in-process ftp server never started listening");
}

/// FAKE FTP server (raw TCP): responds with the greeting, `331` to USER and
/// the `pass_reply` line to PASS. Lets us simulate hostile/strict servers
/// that libunftp does not allow (echoing the password, 530...).
async fn server_fake(pass_reply: &'static str) -> u16 {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("fake server bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (rd, mut wr) = stream.into_split();
                let mut lines = BufReader::new(rd).lines();
                let _ = wr.write_all(b"220 fake ftp\r\n").await;
                while let Ok(Some(line)) = lines.next_line().await {
                    let reply = if line.starts_with("USER") {
                        "331 password please\r\n".to_string()
                    } else if line.starts_with("PASS") {
                        format!("{pass_reply}\r\n")
                    } else if line.starts_with("AUTH") {
                        "500 no tls\r\n".to_string()
                    } else {
                        "502 not implemented\r\n".to_string()
                    };
                    if wr.write_all(reply.as_bytes()).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    port
}

fn spec(port: u16, tls: TlsMode) -> ConnectionSpec {
    ConnectionSpec {
        url: format!("ftp://anonymous@127.0.0.1:{port}"),
        auth: AuthMethod::Password,
        key: None,
        tls,
        ..Default::default()
    }
}

fn secret_anon() -> Secret {
    Secret::new("anonymous".into())
}

/// `unwrap_err` is no good: suppaftp's stream does not implement `Debug`.
async fn connect_err(
    conn: &FtpConnector,
    spec: &ConnectionSpec,
    secret: Option<&Secret>,
) -> ConnectError {
    match conn.connect(spec, secret).await {
        Ok(_) => panic!("expected a connect error"),
        Err(e) => e,
    }
}

/// Real FTPS: `tls = "require"` + extra CA (the server's self-signed one).
/// Login travels ALREADY encrypted and the session stays operational (pwd
/// responds).
#[tokio::test]
async fn ftps_require_connects_and_validates_cert() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = cert_de_test(dir.path());
    let port = server(dir.path(), Some((cert.clone(), key))).await;
    let conn = FtpConnector {
        extra_root_ca: Some(cert),
    };
    let mut out = conn
        .connect(&spec(port, TlsMode::Require), Some(&secret_anon()))
        .await
        .unwrap();
    assert!(!out.tls_degraded, "require never degrades");
    let pwd = out
        .stream
        .pwd()
        .await
        .expect("session operational after FTPS");
    assert_eq!(pwd, "/");
}

/// `require` against a cert that is NOT in the roots (nor in the extra CA):
/// rustls validation takes it down — never degrades or logs in the clear.
#[tokio::test]
async fn require_rejects_an_unknown_cert() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = cert_de_test(dir.path());
    let port = server(dir.path(), Some((cert, key))).await;
    let conn = FtpConnector {
        extra_root_ca: None, // without the self-signed's CA
    };
    let err = connect_err(&conn, &spec(port, TlsMode::Require), Some(&secret_anon())).await;
    assert!(matches!(err, ConnectError::Tls(_)), "was {err:?}");
}

/// `require` against a server WITHOUT TLS: error, no silent fallback.
#[tokio::test]
async fn require_against_a_server_without_tls_fails() {
    let dir = tempfile::tempdir().unwrap();
    let port = server(dir.path(), None).await;
    let conn = FtpConnector::default();
    let err = connect_err(&conn, &spec(port, TlsMode::Require), Some(&secret_anon())).await;
    assert!(matches!(err, ConnectError::Tls(_)), "was {err:?}");
}

/// `allow` degrades to plain (with a tracing warning) if the server gives no
/// TLS.
#[tokio::test]
async fn allow_degrades_to_plaintext_if_there_is_no_tls() {
    let dir = tempfile::tempdir().unwrap();
    let port = server(dir.path(), None).await;
    let conn = FtpConnector::default();
    let mut out = conn
        .connect(&spec(port, TlsMode::Allow), Some(&secret_anon()))
        .await
        .unwrap();
    // #44: the degradation is SIGNALED (the core surfaces it to the user).
    assert!(
        out.tls_degraded,
        "allow fell back to plain → must flag the degradation"
    );
    assert_eq!(
        out.stream.pwd().await.expect("plain session operational"),
        "/"
    );
}

/// `plain` is an explicit opt-in: connects in the clear (with a tracing
/// warning).
#[tokio::test]
async fn plain_es_optin_explicito() {
    let dir = tempfile::tempdir().unwrap();
    let port = server(dir.path(), None).await;
    let conn = FtpConnector::default();
    let mut out = conn
        .connect(&spec(port, TlsMode::Plain), Some(&secret_anon()))
        .await
        .unwrap();
    // plain is plain by choice, NOT a degradation (#44).
    assert!(!out.tls_degraded, "plain is not a degradation");
    assert_eq!(
        out.stream.pwd().await.expect("plain session operational"),
        "/"
    );
}

/// Wrong password (530) → `AuthFailed { user, host }`, no secret.
#[tokio::test]
async fn password_incorrecta_es_auth_failed() {
    let port = server_fake("530 Not logged in").await;
    let conn = FtpConnector::default();
    let err = connect_err(
        &conn,
        &spec(port, TlsMode::Plain),
        Some(&Secret::new("wrong".into())),
    )
    .await;
    let ConnectError::AuthFailed { user, host } = &err else {
        panic!("expected AuthFailed, was {err:?}");
    };
    assert_eq!(user, "anonymous");
    assert_eq!(host, "127.0.0.1");
    assert!(!format!("{err}").contains("wrong"));
}

/// A HOSTILE server that ECHOES the password in its response to PASS: the
/// echo never reaches the error (which would end up in logs — rule 10). The
/// server already knew the secret; the sink to protect is the local log.
#[tokio::test]
async fn server_echo_does_not_reach_the_error() {
    let port = server_fake("500 password 'hunter2-ftp' rejected on a whim").await;
    let conn = FtpConnector::default();
    let err = connect_err(
        &conn,
        &spec(port, TlsMode::Plain),
        Some(&Secret::new("hunter2-ftp".into())),
    )
    .await;
    let msg = format!("{err}");
    assert!(
        !msg.contains("hunter2-ftp"),
        "the error interpolates the server's body: {msg}"
    );
    assert!(
        !msg.contains("whim"),
        "the error interpolates the server's body: {msg}"
    );
}

/// `allow` + server WITH TLS but an invalid cert: fail-closed (signal of an
/// active MITM), do NOT degrade to plain with the same credentials. The
/// legitimate degradation is only when the server REJECTS the AUTH command.
#[tokio::test]
async fn allow_with_invalid_cert_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = cert_de_test(dir.path());
    let port = server(dir.path(), Some((cert, key))).await;
    let conn = FtpConnector {
        extra_root_ca: None, // the self-signed one does NOT validate
    };
    let err = connect_err(&conn, &spec(port, TlsMode::Allow), Some(&secret_anon())).await;
    assert!(
        matches!(err, ConnectError::Tls(_)),
        "must fail closed, not degrade: was {err:?}"
    );
}

/// A corrupt `extra_root_ca` = LOCAL config error, before touching the
/// network (a typo in the CA must not end up sending credentials in the
/// clear).
#[tokio::test]
async fn ca_extra_corrupta_es_error_local() {
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("rota.pem");
    std::fs::write(&ca, "this is not PEM").unwrap();
    let conn = FtpConnector {
        extra_root_ca: Some(ca),
    };
    // Closed port 1: if it touched the network before validating the CA, the
    // error would be Ftp (connection), not Tls (config).
    let spec = ConnectionSpec {
        url: "ftp://u@127.0.0.1:1".into(),
        auth: AuthMethod::Password,
        key: None,
        tls: TlsMode::Allow,
        ..Default::default()
    };
    let err = connect_err(&conn, &spec, Some(&Secret::new("x".into()))).await;
    assert!(matches!(err, ConnectError::Tls(_)), "was {err:?}");
}

/// `auth = "agent"` on FTP = anonymous (guest convention), no secret.
#[tokio::test]
async fn agent_is_anonymous_without_a_secret() {
    let dir = tempfile::tempdir().unwrap();
    let port = server(dir.path(), None).await;
    let conn = FtpConnector::default();
    let spec = ConnectionSpec {
        url: format!("ftp://127.0.0.1:{port}"),
        auth: AuthMethod::Agent,
        key: None,
        tls: TlsMode::Plain,
        ..Default::default()
    };
    let mut out = conn.connect(&spec, None).await.unwrap();
    assert_eq!(out.stream.pwd().await.expect("anonymous session"), "/");
}

/// `auth = "key"` does not exist in FTP: local config error, without
/// touching the network.
#[tokio::test]
async fn auth_key_does_not_apply_to_ftp() {
    let conn = FtpConnector::default();
    let spec = ConnectionSpec {
        // Closed port 1: if connect touched the network, the error would be
        // Ftp.
        url: "ftp://u@127.0.0.1:1".into(),
        auth: AuthMethod::Key,
        key: Some("/no/importa".into()),
        tls: TlsMode::Plain,
        ..Default::default()
    };
    let err = connect_err(&conn, &spec, None).await;
    assert!(matches!(err, ConnectError::Config(_)), "was {err:?}");
}

/// The FTP connector does not accept sftp:// (and vice versa, covered in
/// ssh.rs).
#[tokio::test]
async fn scheme_no_ftp_es_error() {
    let conn = FtpConnector::default();
    let spec = ConnectionSpec {
        url: "sftp://u@h:22".into(),
        auth: AuthMethod::Password,
        key: None,
        tls: TlsMode::Require,
        ..Default::default()
    };
    let err = connect_err(&conn, &spec, None).await;
    assert!(matches!(err, ConnectError::InvalidUrl(_)), "was {err:?}");
}
