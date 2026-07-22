//! `provider_contract!` sobre el guest FTP-por-plugin (#30 stage 3c, ADR 0033)
//! contra un servidor `libunftp` IN-PROCESS — la MISMA suite que pasaban
//! `MemProvider`, `SftpProvider` y el difunto `norte-vfs-ftp`, ahora sobre el
//! provider ejecutándose en WASM. Usa el artefacto `.wasm` EMBEBIDO en
//! `norte-core`, así que valida exactamente lo que se envía.
//!
//! Solo-Linux (como el contrato sftp/ftp original): `libunftp` mapea las ops
//! sobre el FS del host, cuya fidelidad exige un FS POSIX (case-sensitive,
//! byte-preserving). El provider es OS-agnóstico.
//!
//! A diferencia de los E2E de wasm que hacen SKIP, `provider_contract!` NO sabe
//! saltar: este test REQUIERE el runtime wasmtime (siempre presente) + el
//! artefacto embebido (siempre presente). No compila ni ejecuta ningún guest en
//! tiempo de test — el `.wasm` ya está dentro del binario.
#![cfg(target_os = "linux")]

use std::net::TcpStream;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Duration;

use norte_core::ftp_plugin::connect_ftp_plugin;
use norte_core::plugin_provider::PluginProvider;
use norte_proto::{Scheme, VPath};
use norte_vfs::Provider;

/// Arranca `libunftp` sobre `home` en un puerto efímero (hilo con su propio
/// runtime tokio) y devuelve el puerto. Espera a que escuche. Copiado del
/// helper `spawn_ftp_server` de `norte-plugin-host/tests/ftp_plugin_e2e.rs`.
fn spawn_libunftp(home: PathBuf) -> u16 {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind efímero");
    let port = probe.local_addr().expect("addr").port();
    drop(probe);

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async move {
            let server = libunftp::ServerBuilder::new(Box::new(move || {
                unftp_sbe_fs::Filesystem::new(home.clone()).expect("fs backend")
            }))
            .greeting("norte ftp-por-plugin contract")
            .build()
            .expect("build server");
            let _ = server.listen(format!("127.0.0.1:{port}")).await;
        });
    });

    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return port;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("el servidor ftp no arrancó en :{port}");
}

/// Un provider FTP-por-plugin FRESCO sobre un tempdir + servidor libunftp
/// in-process, conectado por el wiring real (resuelve → net → configure).
async fn fresh() -> PluginProvider {
    let dir = tempfile::tempdir().expect("tempdir");
    let port = spawn_libunftp(dir.path().to_path_buf());
    // El tempdir vive tanto como el provider (tests efímeros; el SO limpia /tmp).
    std::mem::forget(dir);
    connect_ftp_plugin("127.0.0.1", port, "anonymous", "anonymous", "/")
        .await
        .expect("provider ftp-por-plugin conectado")
}

/// Provider + el `TempDir` VIVO (para tests que siembran ficheros en el FS del
/// servidor por debajo — nombres no-UTF8 que el provider jamás crearía).
async fn fresh_keep() -> (PluginProvider, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let port = spawn_libunftp(dir.path().to_path_buf());
    let provider = connect_ftp_plugin("127.0.0.1", port, "anonymous", "anonymous", "/")
        .await
        .expect("provider ftp-por-plugin conectado");
    (provider, dir)
}

/// La raíz del `PluginProvider` es SCHEME-ONLY (sin authority) cuando se usa
/// directamente; el adapter también tolera authority (los paths que el engine
/// enruta la llevan, encoding H1).
fn ftp_root() -> VPath {
    VPath::root(Scheme::new("ftp").expect("scheme ftp"), None)
}

/// El corpus hostil compartido MÁS dos nombres específicos de la extracción FTP
/// que el difunto `norte-vfs-ftp/tests/hostile.rs` cubría y el corpus no tiene
/// (encoding M3a): un `;` (que el extractor MLSD crudo `split_once(' ')` NO debe
/// truncar — suppaftp lo truncaría con su `split(';')`) y un espacio inicial
/// (MLSD lo preserva; LIST lo perdería).
fn hostile_names() -> Vec<Vec<u8>> {
    let mut names: Vec<Vec<u8>> = norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        .collect();
    names.push(b"a;b.txt".to_vec());
    names.push(b" sp.txt".to_vec());
    names
}

norte_vfs::provider_contract! {
    mod ftp_plugin_inproc,
    factory: fresh().await,
    root: ftp_root(),
    hostile_names: hostile_names(),
}

/// CR/LF en un segmento = intento de inyección de comando FTP
/// (`x\r\nDELE victima`): el guest lo rechaza LIMPIO como `InvalidPath`, jamás
/// ejecuta el comando colado (encoding M3c; el `Segment` admite `\r`/`\n`, así
/// que el path se construye y llega al guest).
#[tokio::test]
async fn crlf_en_segmento_es_invalid_path() {
    let p = fresh().await;
    let root = ftp_root();
    for probe in [b"x\r\nDELE victima".as_slice(), b"y\nNOOP".as_slice()] {
        let seg = norte_proto::Segment::new(probe.to_vec()).expect("segmento con CR/LF válido");
        let path = root.join(seg);
        assert_eq!(
            p.stat(&path).await.unwrap_err(),
            norte_proto::Error::InvalidPath,
            "stat con CR/LF debe ser InvalidPath: {:?}",
            String::from_utf8_lossy(probe)
        );
        match p.write(&path).await {
            Err(norte_proto::Error::InvalidPath) => {}
            Err(e) => panic!("write con CR/LF debía ser InvalidPath, fue {e:?}"),
            Ok(_) => panic!("write con CR/LF debía ser InvalidPath, abrió el sink"),
        }
    }
}

/// Un nombre NO-UTF8 (`caf\xE9.txt`, 0xE9 crudo) sembrado DIRECTAMENTE en el FS
/// del servidor: suppaftp lo decodifica lossy (U+FFFD); el guest lo salta y el
/// listado JAMÁS emite un `Entry` corrupto ni los bytes 0xEF 0xBF 0xBD (encoding
/// M3b — el provider nunca crearía ese nombre, así que solo un fichero sembrado
/// por fuera lo ejercita).
#[tokio::test]
async fn nombre_no_utf8_en_servidor_no_se_corrompe() {
    use futures::StreamExt;
    let (p, dir) = fresh_keep().await;
    // 0xE9 = é en latin-1; NO es UTF-8 válido.
    let raw_name = b"caf\xE9.txt";
    let mut path = dir.path().to_path_buf();
    path.push(std::ffi::OsStr::from_bytes(raw_name));
    std::fs::write(&path, b"x").expect("sembrar el fichero no-UTF8 en el FS del servidor");

    // list de la raíz: o falla LIMPIO (InvalidPath) o salta la entrada, pero
    // NUNCA devuelve un nombre con U+FFFD.
    let root = ftp_root();
    let mut stream = p.list(&root).await.expect("list abre");
    let mut saw_replacement = false;
    while let Some(entry) = stream.next().await {
        match entry {
            Ok(e) => {
                let bytes = e.path.file_name().expect("con nombre").as_bytes().to_vec();
                if bytes.windows(3).any(|w| w == [0xEF, 0xBF, 0xBD]) {
                    saw_replacement = true;
                }
            }
            // Rechazo limpio del listado por el nombre lossy: aceptable.
            Err(norte_proto::Error::InvalidPath) => {}
            Err(e) => panic!("error inesperado listando: {e:?}"),
        }
    }
    assert!(
        !saw_replacement,
        "el listado nunca debe emitir un nombre con U+FFFD (0xEF 0xBF 0xBD)"
    );
    drop(dir);
}
