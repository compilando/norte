//! M2 (#30, ADR 0033): a server that accepts and stays SILENT hangs the
//! guest's op on the socket; the adapter's per-op timeout cuts it and marks
//! the provider dead, and the NEXT op fails fast (it does not wait out
//! another timeout).
#![cfg(target_os = "linux")]

use std::time::Duration;

use norte_core::plugin_provider::PluginProvider;
use norte_plugin_host::{Capabilities as HostCaps, PluginRuntime};

const FTP_WASM: &[u8] = include_bytes!("../resources/ftp-provider.wasm");

#[tokio::test]
async fn a_blocked_op_times_out_and_marks_it_dead() {
    // Listener that accepts and never responds: the guest's login hangs reading.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        // Accepts and keeps the connections open without speaking.
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

    // configure() hangs at login → times out at ~300 ms → ProviderUnavailable.
    let t0 = std::time::Instant::now();
    let err = provider
        .configure(
            format!("127.0.0.1:{port}"),
            "anonymous".to_owned(),
            "anonymous".to_owned(),
            "/".to_owned(),
        )
        .await
        .expect_err("configure must time out");
    assert!(
        matches!(err, norte_proto::Error::ProviderUnavailable { .. }),
        "was {err:?}"
    );
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "it timed out fast, it did not hang: {:?}",
        t0.elapsed()
    );

    // 2nd op: the provider is dead → fails IMMEDIATELY (not another 300 ms timeout).
    let t1 = std::time::Instant::now();
    let err2 = provider
        .configure(
            format!("127.0.0.1:{port}"),
            "anonymous".to_owned(),
            "anonymous".to_owned(),
            "/".to_owned(),
        )
        .await
        .expect_err("the 2nd op also fails");
    assert!(matches!(
        err2,
        norte_proto::Error::ProviderUnavailable { .. }
    ));
    assert!(
        t1.elapsed() < Duration::from_millis(100),
        "the 2nd op was immediate (dead flag), it took {:?}",
        t1.elapsed()
    );
}
