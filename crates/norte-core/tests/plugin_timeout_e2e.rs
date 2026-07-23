//! M2 (#30, ADR 0033): un servidor que acepta y CALLA cuelga la op del guest en
//! el socket; el timeout por-op del adapter la corta y marca el provider muerto,
//! y la SIGUIENTE op falla rápido (no espera otro timeout).
#![cfg(target_os = "linux")]

use std::time::Duration;

use norte_core::plugin_provider::PluginProvider;
use norte_plugin_host::{Capabilities as HostCaps, PluginRuntime};

const FTP_WASM: &[u8] = include_bytes!("../resources/ftp-provider.wasm");

#[tokio::test]
async fn op_bloqueada_expira_y_marca_muerto() {
    // Listener que acepta y jamás responde: el login del guest se cuelga leyendo.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        // Acepta y retiene las conexiones abiertas sin hablar.
        let mut held = Vec::new();
        while let Ok((sock, _)) = listener.accept() {
            held.push(sock);
        }
    });

    let runtime = PluginRuntime::new().expect("runtime");
    let provider = PluginProvider::from_bytes(
        runtime,
        FTP_WASM,
        HostCaps::with_net(vec!["127.0.0.1".to_owned()]),
        "ftp",
    )
    .expect("provider")
    .with_op_timeout(Duration::from_millis(300));

    // configure() cuelga en el login → expira ~300 ms → ProviderUnavailable.
    let t0 = std::time::Instant::now();
    let err = provider
        .configure(
            format!("127.0.0.1:{port}"),
            "anonymous".to_owned(),
            "anonymous".to_owned(),
            "/".to_owned(),
        )
        .await
        .expect_err("configure debe expirar");
    assert!(
        matches!(err, norte_proto::Error::ProviderUnavailable { .. }),
        "fue {err:?}"
    );
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "expiró rápido, no colgó: {:?}",
        t0.elapsed()
    );

    // 2ª op: el provider está muerto → falla INMEDIATA (no otro timeout de 300 ms).
    let t1 = std::time::Instant::now();
    let err2 = provider
        .configure(
            format!("127.0.0.1:{port}"),
            "anonymous".to_owned(),
            "anonymous".to_owned(),
            "/".to_owned(),
        )
        .await
        .expect_err("2ª op también falla");
    assert!(matches!(
        err2,
        norte_proto::Error::ProviderUnavailable { .. }
    ));
    assert!(
        t1.elapsed() < Duration::from_millis(100),
        "la 2ª op fue inmediata (dead flag), tardó {:?}",
        t1.elapsed()
    );
}
