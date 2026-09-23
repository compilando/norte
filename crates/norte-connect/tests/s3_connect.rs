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
        allow_rsa: false,
        secret: norte_connect::SecretSource::Stored,
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

/// #320 en la capa que TENÍA el fallo: con `auth = "access-key"`, un secreto
/// vacío o un `access_key_id` vacío deben morir aquí, antes de tocar la red.
///
/// El resolver también los rechaza, pero esta es la única prueba que corre
/// contra el conector — API pública de la que el resolver no es el único
/// llamante posible. Si alguien mañana alimenta credenciales desde otro sitio
/// (un flag, un plugin) y esta guarda no está, el fallo vuelve intacto: opendal
/// descarta la cadena vacía en los dos setters, el proveedor estático no se
/// registra y la conexión autentica con lo que ofrezca el entorno.
#[tokio::test]
async fn access_key_o_secret_vacios_son_error_local() {
    let dir = tempfile::tempdir().expect("tempdir");
    let addr = start_s3s(dir.path()).await;

    let vacio = Secret::new(String::new());
    let err = S3Connector::new()
        .connect(&spec_access_key(addr), Some(&vacio))
        .await
        .expect_err("un secret vacío no puede conectar");
    assert!(
        matches!(err, norte_connect::ConnectError::Secret { .. }),
        "fue {err:?}"
    );

    let mut spec = spec_access_key(addr);
    spec.access_key_id = Some(String::new());
    let secret = Secret::new(SK.to_string());
    let err = S3Connector::new()
        .connect(&spec, Some(&secret))
        .await
        .expect_err("un access_key_id vacío no puede conectar");
    assert!(
        matches!(err, norte_connect::ConnectError::Config(_)),
        "fue {err:?}"
    );
}

/// **#321: NINGÚN método de autenticación construye un operador de s3 sin
/// credenciales explícitas, salvo el que las pide a propósito.**
///
/// Lo que hace determinista a `access-key` no es que la cadena ambiente esté
/// apagada: `disable_config_load` solo apaga entorno, perfil e IMDS, y en
/// opendal 0.58 deja dentro SSO, web-identity, process y ECS. Lo que la hace
/// determinista es que el proveedor estático entra por delante y GANA — o
/// sea, que siempre haya credenciales estáticas.
///
/// De ahí este test, que no comprueba un caso sino una PROPIEDAD sobre todo el
/// enum: cada variante o exige credenciales, o es `Agent` —la única que pide
/// la cadena a propósito, para el caso CI/rol de instancia— o se rechaza. El
/// riesgo que cierra es el que la issue nombra: que mañana alguien añada un
/// `auth` nuevo, se olvide de poner credenciales, y la conexión autentique en
/// silencio con la identidad que el entorno ofrezca. Un `match` exhaustivo
/// hace que añadir una variante no compile hasta decidir a qué grupo va.
///
/// No se prueba con variables de entorno hostiles porque `std::env::set_var`
/// es `unsafe` en la edición 2024 y la regla 5 lo prohíbe; se prueba la
/// propiedad que sostiene la garantía, que es la que puede romperse por
/// descuido.
#[tokio::test]
async fn ningun_auth_llega_a_la_cadena_ambiente_por_descuido() {
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
            // La ÚNICA que usa la cadena ambiente, y lo hace porque se le
            // pide: rol de instancia, CI, entorno corporativo.
            AuthMethod::Agent => {}
            // Sin credenciales estáticas no se construye nada: se dice.
            AuthMethod::Key | AuthMethod::Password => {
                let err = S3Connector::new()
                    .connect(&spec, Some(&secret))
                    .await
                    .expect_err("s3 no acepta key/password");
                assert!(
                    matches!(err, norte_connect::ConnectError::Config(_)),
                    "{auth:?}: {err:?}"
                );
            }
            // Exige las dos mitades, y ausentes o vacías es lo mismo.
            AuthMethod::AccessKey => {
                let sin = S3Connector::new()
                    .connect(&spec, None)
                    .await
                    .expect_err("sin secreto no conecta");
                assert!(
                    matches!(sin, norte_connect::ConnectError::Secret { .. }),
                    "{sin:?}"
                );
            }
        }
    }
}

/// #320: `endpoint`/`region` presentes y vacíos no son «sin poner» — opendal
/// los descarta y el destino REAL pasa a ser AWS (y la región, la del entorno).
/// El usuario cree estar hablando con su `MinIO`. Se rechazan al construir.
#[tokio::test]
async fn endpoint_o_region_vacios_son_error_local() {
    let dir = tempfile::tempdir().expect("tempdir");
    let addr = start_s3s(dir.path()).await;
    let secret = Secret::new(SK.to_string());

    for tocar in [0, 1] {
        let mut spec = spec_access_key(addr);
        if tocar == 0 {
            spec.endpoint = Some(String::new());
        } else {
            spec.region = Some(String::new());
        }
        let err = S3Connector::new()
            .connect(&spec, Some(&secret))
            .await
            .expect_err("campo vacío");
        assert!(
            matches!(err, norte_connect::ConnectError::Config(_)),
            "fue {err:?}"
        );
    }
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
        allow_rsa: false,
        secret: norte_connect::SecretSource::Stored,
    };
    let secret = Secret::new(SK.to_string());
    let err = S3Connector::new()
        .connect(&spec, Some(&secret))
        .await
        .expect_err("AWS sin region");
    assert!(matches!(err, norte_connect::ConnectError::Config(_)));
}
