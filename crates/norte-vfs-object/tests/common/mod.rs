//! Harnesses de test del provider object (ADR 0016 J). DOS backends:
//!
//! - [`fs_operator`]: `services-fs` de opendal sobre un tempdir — corre el
//!   CONTRATO completo (misma lógica del provider, sin HTTP). El spike de 7b
//!   midió que s3s-fs es infiel justo donde el contrato aprieta: pierde los
//!   markers de dir vacío al listar y revienta (500) con nombres cerca de
//!   `NAME_MAX` del fs subyacente.
//! - [`s3_operator`]: servidor S3 IN-PROCESS (`s3s` + `s3s-fs` en
//!   `127.0.0.1:puerto-efímero`, patrón libunftp) — corre la suite
//!   S3-ESPECÍFICA (tests/s3.rs): multipart, invisibilidad pre-commit,
//!   conditional write, rangos, nombres. Ahí el spike lo midió FIEL.
//!   Infidelidades medidas de s3s-fs (NO usarlo para esto): markers de dir
//!   vacío invisibles al `LIST`, nombres cerca de `NAME_MAX` → 500,
//!   `HeadObject` de un path-directorio → 500 (S3 real: 404) y `copy_with`
//!   `if_not_exists`
//!   ignorado. Todo lo que toque dirs o keys largas → nightly (`MinIO`).
//!
//! El nightly (feature `it-s3`, tests/reals3.rs) valida contra `MinIO` real.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::Path;

use opendal::Operator;

/// Credenciales de juguete del servidor in-process (no son un secreto).
pub const TEST_AK: &str = "norte-test-ak";
pub const TEST_SK: &str = "norte-test-sk";
pub const TEST_BUCKET: &str = "norte-test";

/// Instala el transporte HTTP por defecto de opendal (idempotente). Con
/// `default-features = false` opendal NO lo auto-registra.
pub fn install_opendal() {
    opendal::install_default();
}

/// `Operator` de `services-fs` enraizado en `base` (tempdir), con
/// `atomic_write_dir` — sin él, el writer de fs escribe DIRECTO al path final
/// y `write_invisible_before_commit` fallaría (en S3 la invisibilidad la da
/// el multipart; aquí la da el tempfile+rename).
pub fn fs_operator(base: &Path, atomic: &Path) -> Operator {
    install_opendal();
    let b = opendal::services::Fs::default()
        .root(base.to_str().expect("tempdir UTF-8"))
        .atomic_write_dir(atomic.to_str().expect("tempdir UTF-8"));
    Operator::new(b).expect("operator fs")
}

/// Arranca un servidor S3 in-process (`s3s-fs` sobre `root`) en un puerto
/// efímero y devuelve su dirección. El bucket [`TEST_BUCKET`] queda creado.
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
        .expect("bind efímero");
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

/// `Operator` S3 (path-style, credenciales de juguete) contra [`start_s3s`].
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
    Operator::new(b).expect("operator s3")
}
