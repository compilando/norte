//! Test de integración NIGHTLY contra un servidor **OpenSSH real** (imagen
//! `atmoz/sftp`, sftp subsystem chrooteado) vía testcontainers (ADR 0013 C2).
//!
//! Fuera del gate de PR: exige Docker y es lento. Lo corre el workflow nightly
//! (`--features it-openssh`), nunca `just ci`. La suite in-process de
//! `contract.rs`/`hostile.rs` ya cubre la lógica en CI normal; esto valida que
//! el provider habla con un servidor de PRODUCCIÓN, no solo con el server de
//! test de russh-sftp (que controlamos nosotros).
//!
//! La conexión SSH aquí es un helper de test MÍNIMO (password auth, host key
//! aceptada): la gestión real de conexión/secretos/host-key es fase 6.
#![cfg(feature = "it-openssh")]

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use norte_proto::{EntryKind, VPath};
use norte_vfs::Provider;
use norte_vfs_sftp::SftpProvider;
use russh::client;
use russh_sftp::client::SftpSession;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt};

const USER: &str = "norte";
const PASS: &str = "s3cr3t";
/// atmoz/sftp: `user:pass:::dir` crea `/dir` ESCRIBIBLE dentro del chroot.
const BASE: &str = "/upload";

/// Handler de cliente russh de test: acepta la host key del contenedor
/// (efímero, de test) y no hace nada más. La verificación real es fase 6.
struct TestClient;

impl client::Handler for TestClient {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        // Contenedor efímero de test: la host key cambia en cada arranque y no
        // hay TOFU aquí. Fase 6 traerá verificación real (known_hosts/keyring).
        Ok(true)
    }
}

/// Establece SSH + abre el subsistema sftp y envuelve la sesión en un
/// `SftpProvider` enraizado en `BASE`.
async fn connect_provider(port: u16) -> SftpProvider {
    let config = Arc::new(client::Config::default());
    let mut handle = client::connect(config, ("127.0.0.1", port), TestClient)
        .await
        .expect("conexión SSH al contenedor");
    let authed = handle
        .authenticate_password(USER, PASS)
        .await
        .expect("auth password");
    assert!(authed.success(), "el servidor rechazó la password de test");

    let channel = handle
        .channel_open_session()
        .await
        .expect("abrir canal de sesión");
    channel
        .request_subsystem(true, "sftp")
        .await
        .expect("solicitar subsistema sftp");
    let session = SftpSession::new(channel.into_stream())
        .await
        .expect("handshake sftp");
    SftpProvider::new(session, BASE)
}

/// Arranca un contenedor `atmoz/sftp` con un usuario de test y devuelve
/// (contenedor, provider). El contenedor vive mientras el guard no se dropee.
async fn setup() -> (testcontainers::ContainerAsync<GenericImage>, SftpProvider) {
    let container = GenericImage::new("atmoz/sftp", "alpine")
        .with_exposed_port(22.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server listening on"))
        .with_cmd([format!("{USER}:{PASS}:::{}", BASE.trim_start_matches('/'))])
        .start()
        .await
        .expect("arrancar contenedor atmoz/sftp");
    let port = container
        .get_host_port_ipv4(22.tcp())
        .await
        .expect("puerto mapeado");
    let provider = connect_provider(port).await;
    (container, provider)
}

fn vp(p: &str) -> VPath {
    VPath::parse(&format!("sftp://127.0.0.1:22{p}")).expect("wire válido")
}

/// Roundtrip básico contra OpenSSH real: escribir, stat, listar, leer.
#[tokio::test]
async fn openssh_write_stat_list_read() {
    let (_c, p) = setup().await;

    let mut sink = p.write(&vp("/hola.txt")).await.expect("write abre");
    sink.write(Bytes::from_static(b"contenido real"))
        .await
        .unwrap();
    sink.commit().await.expect("commit");

    let e = p.stat(&vp("/hola.txt")).await.expect("stat");
    assert_eq!(e.kind, EntryKind::File);
    assert_eq!(e.size, Some(14));

    let mut stream = p.list(&vp("/")).await.expect("list abre");
    let mut visto = false;
    while let Some(item) = stream.next().await {
        let entry = item.expect("entrada válida");
        if entry.path.file_name().map(|n| n.as_bytes()) == Some(b"hola.txt".as_slice()) {
            visto = true;
        }
    }
    assert!(visto, "el archivo escrito aparece en el listado");

    let mut rd = p.read(&vp("/hola.txt"), None).await.expect("read");
    let mut out = Vec::new();
    while let Some(c) = rd.next().await {
        out.extend_from_slice(&c.unwrap());
    }
    assert_eq!(out, b"contenido real");
}

/// Resume por append (ADR 0012) contra OpenSSH real: keep conserva el parcial,
/// una segunda apertura reanuda desde el offset, el commit concatena.
#[tokio::test]
async fn openssh_resume_por_append() {
    let (_c, p) = setup().await;

    let (mut sink, already) = p.open_resumable(&vp("/big.bin")).await.expect("open 1");
    assert_eq!(already, 0);
    sink.write(Bytes::from_static(b"hola")).await.unwrap();
    sink.keep().await.expect("keep conserva el parcial");

    let (mut sink, already) = p.open_resumable(&vp("/big.bin")).await.expect("open 2");
    assert_eq!(already, 4, "reanuda desde lo conservado");
    sink.write(Bytes::from_static(b"mundo")).await.unwrap();
    sink.commit().await.expect("commit");

    let mut rd = p.read(&vp("/big.bin"), None).await.expect("read");
    let mut out = Vec::new();
    while let Some(c) = rd.next().await {
        out.extend_from_slice(&c.unwrap());
    }
    assert_eq!(out, b"holamundo");
}
