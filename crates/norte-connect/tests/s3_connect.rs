//! `S3Connector` contra un servidor S3 in-process (s3s-fs, patrón de
//! norte-vfs-object): el probe valida credenciales/bucket, y el `Operator`
//! resultante opera. Solo-Linux (el harness se respalda en el FS del host).
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

/// spec de access-key contra el endpoint `addr` (path-style: hay endpoint).
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
    }
}

#[tokio::test]
async fn connect_valida_y_opera() {
    let dir = tempfile::tempdir().expect("tempdir");
    let addr = start_s3s(dir.path()).await;
    let secret = Secret::new(SK.to_string());
    let op = S3Connector::new()
        .connect(&spec_access_key(addr), Some(&secret))
        .await
        .expect("connect + probe OK");
    // El Operator resultante opera: write/read roundtrip.
    op.write("hola.txt", b"norte".to_vec())
        .await
        .expect("write");
    let back = op.read("hola.txt").await.expect("read").to_vec();
    assert_eq!(back, b"norte");
}

/// El probe hace I/O REAL: un endpoint muerto (nada escuchando) falla en el
/// `connect`, no en la primera operación — el fail-fast que el timeout del
/// engine espera. (Que un servidor RECHACE credenciales malas exige que valide
/// sigv4 en el list, cosa que s3s-fs no hace → nightly `MinIO`, #50.)
#[tokio::test]
async fn connect_endpoint_muerto_falla_en_el_probe() {
    // Puerto efímero cerrado: bind-then-drop garantiza que nadie escucha.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let dead = probe.local_addr().expect("addr");
    drop(probe);
    let mut spec = spec_access_key(dead);
    spec.endpoint = Some(format!("http://{dead}"));
    let secret = Secret::new(SK.to_string());
    let err = S3Connector::new()
        .connect(&spec, Some(&secret))
        .await
        .expect_err("endpoint muerto debe fallar en el probe");
    let proto: norte_proto::Error = err.into();
    assert!(
        matches!(proto, norte_proto::Error::ProviderUnavailable { .. }),
        "fue {proto:?}"
    );
}

#[tokio::test]
async fn access_key_sin_secret_es_error_local() {
    let dir = tempfile::tempdir().expect("tempdir");
    let addr = start_s3s(dir.path()).await;
    // Sin secret y auth=access-key: error ANTES de tocar la red.
    let err = S3Connector::new()
        .connect(&spec_access_key(addr), None)
        .await
        .expect_err("access-key sin secret");
    assert!(matches!(err, norte_connect::ConnectError::Secret { .. }));
}

/// Regla 10: el secret-access-key JAMÁS aparece en el error (ni en el
/// `ConnectError` ni en su proyección al protocolo), aunque el probe falle.
/// Red que detecta una regresión futura de `map_opendal` (p. ej. a
/// `e.to_string()`).
#[tokio::test]
async fn secret_access_key_nunca_en_el_error() {
    const SECRETO: &str = "AKIA-SECRETO-QUE-NO-DEBE-FILTRARSE-42";
    // Endpoint muerto → el probe falla en la construcción/red.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let dead = probe.local_addr().expect("addr");
    drop(probe);
    let mut spec = spec_access_key(dead);
    spec.endpoint = Some(format!("http://{dead}"));
    let secret = Secret::new(SECRETO.to_string());
    let err = S3Connector::new()
        .connect(&spec, Some(&secret))
        .await
        .expect_err("endpoint muerto");
    let connect_disp = format!("{err}");
    let proto: norte_proto::Error = err.into();
    let proto_disp = format!("{proto:?}");
    assert!(
        !connect_disp.contains(SECRETO) && !proto_disp.contains(SECRETO),
        "el secreto se filtró: connect={connect_disp:?} proto={proto_disp:?}"
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
    };
    let secret = Secret::new(SK.to_string());
    let err = S3Connector::new()
        .connect(&spec, Some(&secret))
        .await
        .expect_err("AWS sin region");
    assert!(matches!(err, norte_connect::ConnectError::Config(_)));
}
