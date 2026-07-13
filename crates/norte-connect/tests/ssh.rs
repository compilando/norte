//! Integración de `SshConnector` contra un servidor SSH **in-process** (russh
//! server, sin Docker): ciclo TOFU completo (desconocida → trust → conocida →
//! mismatch), auth por password / clave ed25519 / agente, rechazo de RSA
//! (ADR 0015 D/E). El nightly con OpenSSH real (`norte-vfs-sftp/openssh.rs`)
//! cubre la interop con un servidor de producción; esto cubre la LÓGICA.

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

/// Clave ed25519 efímera de test.
fn clave() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("generar ed25519")
}

fn fingerprint(pk: &PublicKey) -> String {
    pk.fingerprint(HashAlg::Sha256).to_string()
}

/// Handler del servidor de test: password fija, una pubkey autorizada
/// opcional y subsistema sftp trivial (solo handshake init/version).
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
        Ok(
            if user == USER && Some(public_key) == self.allowed_pubkey.as_ref() {
                Auth::Accept
            } else {
                Auth::reject()
            },
        )
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
                .expect("canal abierto antes del subsistema");
            session.channel_success(channel_id)?;
            tokio::spawn(russh_sftp::server::run(channel.into_stream(), TrivialSftp));
        } else {
            session.channel_failure(channel_id)?;
        }
        Ok(())
    }
}

/// Servidor sftp mínimo: el framework responde el init/version; todo lo demás
/// es `OpUnsupported` (suficiente para probar el handshake de `SftpSession`).
struct TrivialSftp;

impl russh_sftp::server::Handler for TrivialSftp {
    type Error = russh_sftp::protocol::StatusCode;

    fn unimplemented(&self) -> Self::Error {
        russh_sftp::protocol::StatusCode::OpUnsupported
    }
}

/// Arranca el servidor SSH de test en 127.0.0.1:0 y devuelve el puerto.
async fn spawn_server(host_key: PrivateKey, allowed_pubkey: Option<PublicKey>) -> u16 {
    let config = Arc::new(russh::server::Config {
        keys: vec![host_key],
        auth_rejection_time: std::time::Duration::from_millis(10),
        auth_rejection_time_initial: Some(std::time::Duration::ZERO),
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind del servidor de test");
    let port = listener.local_addr().expect("addr local").port();
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

/// Conector con store vacío en un tempdir (TOFU virgen) y sin agente.
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
    }
}

/// Como `unwrap_err`, pero `SftpSession` no implementa `Debug`.
async fn connect_err(
    conn: &SshConnector,
    spec: &ConnectionSpec,
    secret: Option<&Secret>,
) -> ConnectError {
    match conn.connect(spec, secret).await {
        Ok(_) => panic!("esperaba un error de connect"),
        Err(e) => e,
    }
}

/// Ciclo TOFU completo: primer contacto → `HostKeyUnknown` con fingerprint,
/// trust explícito → conexión y handshake sftp reales.
#[tokio::test]
async fn tofu_desconocida_trust_y_conexion() {
    let host_key = clave();
    let fp_real = fingerprint(host_key.public_key());
    let port = spawn_server(host_key, None).await;
    let dir = tempfile::tempdir().unwrap();
    let conn = connector(dir.path());
    let spec = spec_password(port);
    let secret = Secret::new(PASS.into());

    // 1. Primer contacto: NUNCA conecta a ciegas.
    let err = connect_err(&conn, &spec, Some(&secret)).await;
    let ConnectError::HostKeyUnknown {
        host,
        port: p,
        algo,
        fingerprint: fp,
    } = err
    else {
        panic!("esperaba HostKeyUnknown, fue {err:?}");
    };
    assert_eq!(host, "127.0.0.1");
    assert_eq!(p, port);
    assert_eq!(algo, "ssh-ed25519");
    assert_eq!(fp, fp_real);

    // 2. Confirmación explícita (flujo connection.trust_host_key).
    conn.trust_host_key(&host, p, &fp).await.unwrap();

    // 3. Reintento: la clave ya es de confianza; auth + handshake sftp OK.
    let _session = conn.connect(&spec, Some(&secret)).await.unwrap();
}

/// El trust re-verifica el fingerprint contra la clave REAL del host
/// (anti-TOCTOU): un fingerprint que no coincide no registra nada.
#[tokio::test]
async fn trust_con_fingerprint_incorrecto_no_registra() {
    let host_key = clave();
    let port = spawn_server(host_key, None).await;
    let dir = tempfile::tempdir().unwrap();
    let conn = connector(dir.path());
    let fp_falso = fingerprint(clave().public_key());

    let err = conn
        .trust_host_key("127.0.0.1", port, &fp_falso)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ConnectError::HostKeyMismatch { .. }),
        "fue {err:?}"
    );

    // Sigue sin estar registrada: el siguiente connect es primer contacto.
    let err = connect_err(&conn, &spec_password(port), Some(&Secret::new(PASS.into()))).await;
    assert!(
        matches!(err, ConnectError::HostKeyUnknown { .. }),
        "fue {err:?}"
    );
}

/// Una host key REGISTRADA que cambia es `HostKeyMismatch` (posible MITM):
/// jamás se conecta ni se re-registra en silencio.
#[tokio::test]
async fn host_key_cambiada_es_mismatch() {
    let host_key = clave();
    let fp_presentada = fingerprint(host_key.public_key());
    let port = spawn_server(host_key, None).await;
    let dir = tempfile::tempdir().unwrap();
    // El store ya tiene OTRA clave registrada para este host:puerto (formato
    // OpenSSH: se puede pre-poblar a mano, ADR 0015 D).
    let registrada = clave().public_key().to_openssh().unwrap();
    std::fs::write(
        dir.path().join("known_hosts"),
        format!("[127.0.0.1]:{port} {registrada}\n"),
    )
    .unwrap();
    let conn = connector(dir.path());

    let err = connect_err(&conn, &spec_password(port), Some(&Secret::new(PASS.into()))).await;
    let ConnectError::HostKeyMismatch {
        fingerprint: fp, ..
    } = err
    else {
        panic!("esperaba HostKeyMismatch, fue {err:?}");
    };
    // El fingerprint reportado es el de la clave PRESENTADA (la sospechosa).
    assert_eq!(fp, fp_presentada);
}

/// Password incorrecta → `AuthFailed` (sin material secreto en el error).
#[tokio::test]
async fn password_incorrecta_es_auth_failed() {
    let host_key = clave();
    let fp = fingerprint(host_key.public_key());
    let port = spawn_server(host_key, None).await;
    let dir = tempfile::tempdir().unwrap();
    let conn = connector(dir.path());
    conn.trust_host_key("127.0.0.1", port, &fp).await.unwrap();

    let err = connect_err(
        &conn,
        &spec_password(port),
        Some(&Secret::new("mala".into())),
    )
    .await;
    let ConnectError::AuthFailed { user, host } = &err else {
        panic!("esperaba AuthFailed, fue {err:?}");
    };
    assert_eq!(user, USER);
    assert_eq!(host, "127.0.0.1");
    assert!(
        !format!("{err}").contains("mala"),
        "el error no debe llevar la password"
    );
}

/// Auth por clave ed25519 CIFRADA con passphrase: la passphrase llega como
/// `Secret` (resuelta por el `SecretResolver` en el flujo real).
#[tokio::test]
async fn auth_por_clave_ed25519_cifrada() {
    let host_key = clave();
    let fp = fingerprint(host_key.public_key());
    let client_key = clave();
    let port = spawn_server(host_key, Some(client_key.public_key().clone())).await;
    let dir = tempfile::tempdir().unwrap();
    let conn = connector(dir.path());
    conn.trust_host_key("127.0.0.1", port, &fp).await.unwrap();

    let cifrada = client_key
        .encrypt(&mut rand::rng(), "frase-de-paso")
        .expect("cifrar clave de test");
    let key_path = dir.path().join("id_ed25519");
    std::fs::write(&key_path, cifrada.to_openssh(LineEnding::LF).unwrap()).unwrap();

    let spec = ConnectionSpec {
        url: format!("sftp://{USER}@127.0.0.1:{port}"),
        auth: AuthMethod::Key,
        key: Some(key_path.clone()),
        tls: TlsMode::Require,
    };
    let _session = conn
        .connect(&spec, Some(&Secret::new("frase-de-paso".into())))
        .await
        .unwrap();

    // Passphrase incorrecta → KeyLoad claro (y sin passphrase en el mensaje).
    let err = connect_err(&conn, &spec, Some(&Secret::new("otra".into()))).await;
    assert!(matches!(err, ConnectError::KeyLoad { .. }), "fue {err:?}");
    assert!(!format!("{err}").contains("otra"));
}

/// Una clave RSA se rechaza ANTES de tocar la red, con un error que
/// recomienda ed25519 (ADR 0015 E, cierra #36 / RUSTSEC-2023-0071).
#[tokio::test]
async fn clave_rsa_rechazada_sin_red() {
    // Fixture RSA generada para tests (jamás usada en nada real).
    const RSA_FIXTURE: &str = include_str!("fixtures/id_rsa_test");
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("id_rsa");
    std::fs::write(&key_path, RSA_FIXTURE).unwrap();
    let conn = connector(dir.path());

    let spec = ConnectionSpec {
        // Puerto 1: cerrado — si el connect tocara la red antes de validar la
        // clave, el error sería de conexión, no KeyUnsupported.
        url: format!("sftp://{USER}@127.0.0.1:1"),
        auth: AuthMethod::Key,
        key: Some(key_path),
        tls: TlsMode::Require,
    };
    let err = connect_err(&conn, &spec, None).await;
    let ConnectError::KeyUnsupported { algo, .. } = &err else {
        panic!("esperaba KeyUnsupported, fue {err:?}");
    };
    assert!(algo.contains("rsa"), "algo fue {algo}");
    assert!(
        format!("{err}").contains("ed25519"),
        "el error debe recomendar ed25519"
    );
}

/// Auth vía agente SSH (in-process, mismo protocolo que ssh-agent real).
#[tokio::test]
async fn auth_por_agente() {
    let host_key = clave();
    let fp = fingerprint(host_key.public_key());
    let client_key = clave();
    let port = spawn_server(host_key, Some(client_key.public_key().clone())).await;
    let dir = tempfile::tempdir().unwrap();

    // Agente in-process sobre un socket unix del tempdir.
    let sock = dir.path().join("agent.sock");
    let listener = tokio::net::UnixListener::bind(&sock).expect("bind agente");
    tokio::spawn(russh::keys::agent::server::serve(
        tokio_stream::wrappers::UnixListenerStream::new(listener),
        (),
    ));
    let mut agent = russh::keys::agent::client::AgentClient::connect_uds(&sock)
        .await
        .expect("conectar al agente");
    agent
        .add_identity(&client_key, &[])
        .await
        .expect("añadir identidad");
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
    };
    let _session = conn.connect(&spec, None).await.unwrap();
}

/// Trust sobre un host que YA tiene OTRA clave registrada: `HostKeyMismatch`
/// (la categoría de rotación/MITM del wire) y el store queda intacto. (El
/// `learn` de russh solo appendea: "parchear por encima" dejaría el fichero
/// con dos claves en conflicto y el check en Mismatch perpetuo pese a un
/// trust que reportó éxito.)
#[tokio::test]
async fn trust_sobre_clave_registrada_distinta_es_error() {
    let host_key = clave();
    let fp_nueva = fingerprint(host_key.public_key());
    let port = spawn_server(host_key, None).await;
    let dir = tempfile::tempdir().unwrap();
    let vieja = clave().public_key().to_openssh().unwrap();
    let linea_vieja = format!("[127.0.0.1]:{port} {vieja}\n");
    std::fs::write(dir.path().join("known_hosts"), &linea_vieja).unwrap();
    let conn = connector(dir.path());

    let err = conn
        .trust_host_key("127.0.0.1", port, &fp_nueva)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ConnectError::HostKeyMismatch { .. }),
        "fue {err:?}"
    );
    // El fichero NO cambió: sigue solo la entrada antigua.
    let contenido = std::fs::read_to_string(dir.path().join("known_hosts")).unwrap();
    assert_eq!(contenido, linea_vieja);
}

/// Sin agente disponible, `auth = "agent"` es un error claro, no un cuelgue.
#[tokio::test]
async fn agente_ausente_es_error_claro() {
    let dir = tempfile::tempdir().unwrap();
    let conn = connector(dir.path()); // agent_socket: None
    let spec = ConnectionSpec {
        url: format!("sftp://{USER}@127.0.0.1:1"),
        auth: AuthMethod::Agent,
        key: None,
        tls: TlsMode::Require,
    };
    let err = connect_err(&conn, &spec, None).await;
    assert!(matches!(err, ConnectError::Agent(_)), "fue {err:?}");
}

/// La URL debe ser sftp:// — un scheme ftp aquí es error de uso, no un cuelgue.
#[tokio::test]
async fn scheme_no_sftp_es_error() {
    let dir = tempfile::tempdir().unwrap();
    let conn = connector(dir.path());
    let spec = ConnectionSpec {
        url: "ftp://u@h:21".into(),
        auth: AuthMethod::Password,
        key: None,
        tls: TlsMode::Require,
    };
    let err = connect_err(&conn, &spec, None).await;
    assert!(matches!(err, ConnectError::InvalidUrl(_)), "fue {err:?}");
}
