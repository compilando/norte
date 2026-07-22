//! Integración del [`ConnectionManager`] REAL contra servidores in-process
//! (fase 6e): sftp (russh server) y ftp (libunftp), con `connections.toml` y
//! secretos del `secrets.age` en un tempdir — el camino completo
//! resolución → secreto → transporte → provider, sin Docker.

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

// ---------- servidor SSH in-process (password + sftp trivial) ----------

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
            .expect("lock de test")
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
                .expect("lock de test")
                .remove(&channel_id)
                .expect("canal abierto");
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

/// Arranca el servidor SSH y devuelve (puerto, fingerprint de su host key).
async fn spawn_ssh() -> (u16, String) {
    let host_key =
        russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)
            .expect("host key de test");
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

// ---------- config de test (connections.toml + secrets.age) ----------

/// Puebla el dir de config: la conexión nombrada `name` y su secreto en el
/// `secrets.age` (así se ejercita el resolver real, sin tocar env global).
async fn config_con(dir: &Path, name: &str, url: &str, auth: &str) {
    std::fs::write(
        dir.join("connections.toml"),
        format!("[connections.{name}]\nurl = \"{url}\"\nauth = \"{auth}\"\n"),
    )
    .unwrap();
    let key_path = dir.join("secrets.key");
    std::fs::write(&key_path, "passphrase-de-test").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    SecretResolver::new(dir)
        .store_in_age(name, &Secret::new(PASS.into()))
        .await
        .expect("guardar secreto de test");
}

// ---------- tests ----------

/// Camino completo sftp: primer contacto → `HostKeyUnknown` (fingerprint de la
/// clave REAL), trust vía el manager (re-verifica), connect → provider vivo
/// cacheable. La resolución matchea la entrada nombrada y saca el secreto
/// del `secrets.age`.
#[tokio::test]
async fn sftp_tofu_trust_y_provider() {
    let (port, fp_real) = spawn_ssh().await;
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sftp://{USER}@127.0.0.1:{port}");
    config_con(dir.path(), "trabajo", &url, "password").await;
    let mgr = ConnectionManager::new(dir.path());

    // Primer contacto.
    let err = mgr
        .connect("sftp", &format!("{USER}@127.0.0.1:{port}"))
        .await
        .err()
        .expect("primer contacto debe fallar");
    let Error::HostKeyUnknown {
        host,
        port: p,
        fingerprint,
        ..
    } = &err
    else {
        panic!("esperaba HostKeyUnknown, fue {err:?}");
    };
    assert_eq!(host, "127.0.0.1");
    assert_eq!(*p, Some(port));
    assert_eq!(fingerprint, &fp_real);

    // Confianza explícita (anti-TOCTOU: re-disca y compara) y reintento.
    mgr.trust_host_key(host, *p, fingerprint)
        .await
        .expect("trust");
    let connected = mgr
        .connect("sftp", &format!("{USER}@127.0.0.1:{port}"))
        .await
        .expect("connect tras trust");
    assert_eq!(connected.provider.scheme(), "sftp");
    assert!(connected.warnings.is_empty(), "sftp no degrada TLS");

    // connect_named usa la misma entrada (y la clave ya es de confianza).
    let (scheme, authority, _prov) = mgr.connect_named("trabajo").await.expect("connect_named");
    assert_eq!(scheme, "sftp");
    assert_eq!(authority, format!("{USER}@127.0.0.1:{port}"));

    // named_url resuelve la URL de la entrada; un nombre inexistente es
    // NotFound.
    assert_eq!(named_url(dir.path(), "trabajo").await.unwrap(), url);
    assert!(matches!(
        named_url(dir.path(), "no-existe").await,
        Err(Error::NotFound)
    ));
}

/// Un fingerprint que NO coincide con la clave real no registra nada.
#[tokio::test]
async fn trust_con_huella_falsa_es_mismatch() {
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
    assert!(matches!(err, Error::HostKeyMismatch { .. }), "fue {err:?}");
}

/// Camino ftp (#30 stage 3c, ADR 0033): `ftp://` va por el provider-plugin
/// WASM. El manager resuelve la IP, instancia el guest embebido y lo configura
/// contra el servidor; el provider resultante sirve el scheme `ftp` y SIEMPRE
/// avisa `FtpPlaintext` (FTPS = deuda, la sesión es en claro).
#[tokio::test]
async fn ftp_establece_provider_por_plugin() {
    use libunftp::ServerBuilder;
    use unftp_sbe_fs::Filesystem;

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind efímero");
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
    // FTP-por-plugin es SIEMPRE en claro (FTPS = deuda): un aviso FtpPlaintext.
    assert!(
        connected
            .warnings
            .iter()
            .any(|w| w.reason == ConnectionWarningReason::FtpPlaintext),
        "el ftp-por-plugin avisa FtpPlaintext, fue {:?}",
        connected.warnings
    );
    // Y el provider LISTA de verdad la raíz (el guest configuró y conecta).
    let root = norte_proto::VPath::root(norte_proto::Scheme::new("ftp").unwrap(), None);
    let _stream = norte_vfs::Provider::list(&*connected.provider, &root)
        .await
        .expect("lista la raíz remota");
}

/// Un scheme que el manager no sabe conectar es Unsupported.
#[tokio::test]
async fn scheme_desconocido_es_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = ConnectionManager::new(dir.path());
    let err = mgr
        .connect("gopher", "h")
        .await
        .err()
        .expect("gopher no conecta");
    // La URL gopher://h ni siquiera parsea como conexión remota conocida.
    assert!(
        matches!(err, Error::Unsupported | Error::InvalidPath),
        "fue {err:?}"
    );
}
