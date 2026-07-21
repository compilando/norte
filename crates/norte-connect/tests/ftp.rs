//! Integración de `FtpConnector` contra libunftp **in-process** (sin Docker):
//! política TLS de ADR 0015 F / issue #38 — `require` exige FTPS y valida el
//! cert, `allow` degrada a plano CON aviso, `plain` es opt-in explícito — más
//! login anónimo/password y rechazo de `auth = "key"`.

use std::path::Path;
use std::time::Duration;

use libunftp::ServerBuilder;
use norte_connect::{AuthMethod, ConnectError, ConnectionSpec, FtpConnector, Secret, TlsMode};
use unftp_sbe_fs::Filesystem;

/// Cert self-signed efímero para 127.0.0.1 → (cert.pem, key.pem) en `dir`.
fn cert_de_test(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()])
        .expect("generar cert de test");
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    std::fs::write(&cert_path, cert.cert.pem()).unwrap();
    std::fs::write(&key_path, cert.signing_key.serialize_pem()).unwrap();
    (cert_path, key_path)
}

/// Arranca libunftp in-process (anónimo, backend tempdir) en puerto efímero.
/// Con `ftps = Some((cert, key))` exige AUTH TLS antes del login.
async fn servidor(base: &Path, ftps: Option<(std::path::PathBuf, std::path::PathBuf)>) -> u16 {
    let home = base.to_path_buf();
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind efímero");
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
        // Panic con causa: si el puerto efímero se lo robó otro proceso en la
        // ventana bind-then-drop, el diagnóstico real es este, no el timeout.
        if let Err(e) = server.listen(format!("127.0.0.1:{port}")).await {
            panic!("listen del ftp de test: {e}");
        }
    });
    // Espera activa a que el listener esté vivo.
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return port;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("el servidor ftp in-process no llegó a escuchar");
}

/// Servidor FTP FALSO (TCP crudo): responde el saludo, `331` a USER y la
/// línea `pass_reply` a PASS. Permite simular servidores hostiles/estrictos
/// que libunftp no deja (echo de la password, 530...).
async fn servidor_falso(pass_reply: &'static str) -> u16 {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind del servidor falso");
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

/// `unwrap_err` no vale: el stream de suppaftp no implementa `Debug`.
async fn connect_err(
    conn: &FtpConnector,
    spec: &ConnectionSpec,
    secret: Option<&Secret>,
) -> ConnectError {
    match conn.connect(spec, secret).await {
        Ok(_) => panic!("esperaba un error de connect"),
        Err(e) => e,
    }
}

/// FTPS de verdad: `tls = "require"` + CA extra (self-signed del servidor).
/// El login viaja YA cifrado y la sesión queda operativa (pwd responde).
#[tokio::test]
async fn ftps_require_conecta_y_valida_cert() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = cert_de_test(dir.path());
    let port = servidor(dir.path(), Some((cert.clone(), key))).await;
    let conn = FtpConnector {
        extra_root_ca: Some(cert),
    };
    let mut out = conn
        .connect(&spec(port, TlsMode::Require), Some(&secret_anon()))
        .await
        .unwrap();
    assert!(!out.tls_degraded, "require jamás degrada");
    let pwd = out.stream.pwd().await.expect("sesión operativa tras FTPS");
    assert_eq!(pwd, "/");
}

/// `require` contra un cert que NO está en las raíces (ni en la CA extra):
/// la validación rustls lo tumba — jamás se degrada ni se loguea en claro.
#[tokio::test]
async fn require_rechaza_cert_desconocido() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = cert_de_test(dir.path());
    let port = servidor(dir.path(), Some((cert, key))).await;
    let conn = FtpConnector {
        extra_root_ca: None, // sin la CA del self-signed
    };
    let err = connect_err(&conn, &spec(port, TlsMode::Require), Some(&secret_anon())).await;
    assert!(matches!(err, ConnectError::Tls(_)), "fue {err:?}");
}

/// `require` contra un servidor SIN TLS: error, no fallback silencioso.
#[tokio::test]
async fn require_contra_servidor_sin_tls_falla() {
    let dir = tempfile::tempdir().unwrap();
    let port = servidor(dir.path(), None).await;
    let conn = FtpConnector::default();
    let err = connect_err(&conn, &spec(port, TlsMode::Require), Some(&secret_anon())).await;
    assert!(matches!(err, ConnectError::Tls(_)), "fue {err:?}");
}

/// `allow` degrada a plano (con aviso por tracing) si el servidor no da TLS.
#[tokio::test]
async fn allow_degrada_a_plano_si_no_hay_tls() {
    let dir = tempfile::tempdir().unwrap();
    let port = servidor(dir.path(), None).await;
    let conn = FtpConnector::default();
    let mut out = conn
        .connect(&spec(port, TlsMode::Allow), Some(&secret_anon()))
        .await
        .unwrap();
    // #44: la degradación se SEÑALIZA (el core la surfacea al usuario).
    assert!(
        out.tls_degraded,
        "allow cayó a claro → debe marcar la degradación"
    );
    assert_eq!(out.stream.pwd().await.expect("sesión plana operativa"), "/");
}

/// `plain` es opt-in explícito: conecta en claro (con aviso por tracing).
#[tokio::test]
async fn plain_es_optin_explicito() {
    let dir = tempfile::tempdir().unwrap();
    let port = servidor(dir.path(), None).await;
    let conn = FtpConnector::default();
    let mut out = conn
        .connect(&spec(port, TlsMode::Plain), Some(&secret_anon()))
        .await
        .unwrap();
    // plain es claro por elección, NO una degradación (#44).
    assert!(!out.tls_degraded, "plain no es una degradación");
    assert_eq!(out.stream.pwd().await.expect("sesión plana operativa"), "/");
}

/// Password incorrecta (530) → `AuthFailed { user, host }`, sin secreto.
#[tokio::test]
async fn password_incorrecta_es_auth_failed() {
    let port = servidor_falso("530 Not logged in").await;
    let conn = FtpConnector::default();
    let err = connect_err(
        &conn,
        &spec(port, TlsMode::Plain),
        Some(&Secret::new("mala".into())),
    )
    .await;
    let ConnectError::AuthFailed { user, host } = &err else {
        panic!("esperaba AuthFailed, fue {err:?}");
    };
    assert_eq!(user, "anonymous");
    assert_eq!(host, "127.0.0.1");
    assert!(!format!("{err}").contains("mala"));
}

/// Un servidor HOSTIL que ECOA la password en su respuesta a PASS: el echo
/// jamás llega al error (que acabaría en logs — regla 10). El servidor ya
/// conocía el secreto; el sink a proteger es el log local.
#[tokio::test]
async fn echo_del_servidor_no_llega_al_error() {
    let port = servidor_falso("500 password 'hunter2-ftp' rechazada por capricho").await;
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
        "el error interpola el body del servidor: {msg}"
    );
    assert!(
        !msg.contains("capricho"),
        "el error interpola el body del servidor: {msg}"
    );
}

/// `allow` + servidor CON TLS pero cert inválido: fail-closed (señal de MITM
/// activo), NO degradar a plano con las mismas credenciales. La degradación
/// legítima es solo cuando el servidor RECHAZA el comando AUTH.
#[tokio::test]
async fn allow_con_cert_invalido_falla_cerrado() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = cert_de_test(dir.path());
    let port = servidor(dir.path(), Some((cert, key))).await;
    let conn = FtpConnector {
        extra_root_ca: None, // el self-signed NO valida
    };
    let err = connect_err(&conn, &spec(port, TlsMode::Allow), Some(&secret_anon())).await;
    assert!(
        matches!(err, ConnectError::Tls(_)),
        "debe fallar cerrado, no degradar: fue {err:?}"
    );
}

/// `extra_root_ca` corrupta = error LOCAL de config, antes de tocar la red
/// (un typo en la CA no puede acabar mandando credenciales en claro).
#[tokio::test]
async fn ca_extra_corrupta_es_error_local() {
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("rota.pem");
    std::fs::write(&ca, "esto no es PEM").unwrap();
    let conn = FtpConnector {
        extra_root_ca: Some(ca),
    };
    // Puerto 1 cerrado: si tocara la red antes de validar la CA, el error
    // sería Ftp (conexión), no Tls (config).
    let spec = ConnectionSpec {
        url: "ftp://u@127.0.0.1:1".into(),
        auth: AuthMethod::Password,
        key: None,
        tls: TlsMode::Allow,
        ..Default::default()
    };
    let err = connect_err(&conn, &spec, Some(&Secret::new("x".into()))).await;
    assert!(matches!(err, ConnectError::Tls(_)), "fue {err:?}");
}

/// `auth = "agent"` en FTP = anónimo (convención guest), sin secreto.
#[tokio::test]
async fn agent_es_anonimo_sin_secreto() {
    let dir = tempfile::tempdir().unwrap();
    let port = servidor(dir.path(), None).await;
    let conn = FtpConnector::default();
    let spec = ConnectionSpec {
        url: format!("ftp://127.0.0.1:{port}"),
        auth: AuthMethod::Agent,
        key: None,
        tls: TlsMode::Plain,
        ..Default::default()
    };
    let mut out = conn.connect(&spec, None).await.unwrap();
    assert_eq!(out.stream.pwd().await.expect("sesión anónima"), "/");
}

/// `auth = "key"` no existe en FTP: error de config local, sin tocar la red.
#[tokio::test]
async fn auth_key_no_aplica_a_ftp() {
    let conn = FtpConnector::default();
    let spec = ConnectionSpec {
        // Puerto 1 cerrado: si el connect tocara la red, el error sería Ftp.
        url: "ftp://u@127.0.0.1:1".into(),
        auth: AuthMethod::Key,
        key: Some("/no/importa".into()),
        tls: TlsMode::Plain,
        ..Default::default()
    };
    let err = connect_err(&conn, &spec, None).await;
    assert!(matches!(err, ConnectError::Config(_)), "fue {err:?}");
}

/// El conector FTP no acepta sftp:// (y viceversa, cubierto en ssh.rs).
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
    assert!(matches!(err, ConnectError::InvalidUrl(_)), "fue {err:?}");
}
