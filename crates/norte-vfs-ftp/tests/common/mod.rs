//! Servidor FTP IN-PROCESS para los tests (ADR 0014, C): `libunftp` +
//! `unftp-sbe-fs` respaldado por un `tempdir`, en un puerto efímero de
//! localhost. FTP exige sockets reales (control + datos PASV), así que no vale
//! un `duplex` como en sftp — pero sigue sin Docker. Permite correr la suite
//! contractual COMPLETA en CI normal.

#![allow(dead_code)]

use std::path::Path;
use std::time::Duration;

use libunftp::ServerBuilder;
use suppaftp::tokio::AsyncRustlsFtpStream;
use unftp_sbe_fs::Filesystem;

/// Arranca un servidor FTP in-process respaldado por `base` (tempdir) en un
/// puerto efímero de localhost y devuelve una conexión de cliente logueada
/// (auth anónima: el authenticator por defecto de libunftp acepta cualquiera).
pub async fn connect(base: &Path) -> AsyncRustlsFtpStream {
    let home = base.to_path_buf();
    // Puerto efímero: bind-then-drop para descubrirlo (ventana de carrera
    // mínima en localhost; el bucle de connect de abajo la absorbe).
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind efímero");
    let port = probe.local_addr().expect("addr").port();
    drop(probe);

    let server = ServerBuilder::new(Box::new(move || {
        Filesystem::new(home.clone()).expect("fs backend")
    }))
    .greeting("norte test ftp")
    .build()
    .expect("build server");
    tokio::spawn(async move {
        let _ = server.listen(format!("127.0.0.1:{port}")).await;
    });

    // Reintenta hasta que el servidor esté escuchando (el listen del task tarda
    // un instante en bindear).
    let addr = format!("127.0.0.1:{port}");
    let mut last = None;
    for _ in 0..100 {
        match AsyncRustlsFtpStream::connect(&addr).await {
            Ok(mut ftp) => {
                ftp.login("anonymous", "anonymous").await.expect("login");
                return ftp;
            }
            Err(e) => {
                last = Some(e);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
    panic!("no se pudo conectar al servidor ftp in-process: {last:?}");
}
