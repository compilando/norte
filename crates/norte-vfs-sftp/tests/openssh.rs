//! NIGHTLY integration test against a **real OpenSSH server** (`atmoz/sftp`
//! image, chrooted sftp subsystem) via testcontainers (ADR 0013 C2).
//!
//! Outside the PR gate: it requires Docker and is slow. The nightly workflow
//! runs it (`--features it-openssh`), never `just ci`. The in-process suite
//! of `contract.rs`/`hostile.rs` already covers the logic in normal CI; this
//! validates that the provider talks to a PRODUCTION server, not only to
//! russh-sftp's test server (which we control).
//!
//! The SSH connection here is a MINIMAL test helper (password auth, host key
//! accepted): real connection/secrets/host-key management is phase 6.
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
/// atmoz/sftp: `user:pass:::dir` creates `/dir` WRITABLE inside the chroot.
const BASE: &str = "/upload";

/// Test russh client handler: accepts the container's host key (ephemeral,
/// test-only) and does nothing else. Real verification is phase 6.
struct TestClient;

impl client::Handler for TestClient {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        // Ephemeral test container: the host key changes on every start and
        // there is no TOFU here. Phase 6 will bring real verification
        // (known_hosts/keyring).
        Ok(true)
    }
}

/// Establishes SSH + opens the sftp subsystem and wraps the session in an
/// `SftpProvider` rooted at `BASE`.
async fn connect_provider(port: u16) -> SftpProvider {
    let config = Arc::new(client::Config::default());
    let mut handle = client::connect(config, ("127.0.0.1", port), TestClient)
        .await
        .expect("SSH connection to the container");
    let authed = handle
        .authenticate_password(USER, PASS)
        .await
        .expect("password auth");
    assert!(authed.success(), "the server rejected the test password");

    let channel = handle
        .channel_open_session()
        .await
        .expect("open session channel");
    channel
        .request_subsystem(true, "sftp")
        .await
        .expect("request sftp subsystem");
    let session = SftpSession::new(channel.into_stream())
        .await
        .expect("sftp handshake");
    SftpProvider::new(session, BASE)
}

/// Starts an `atmoz/sftp` container with a test user and returns
/// (container, provider). The container lives as long as the guard is not
/// dropped.
async fn setup() -> (testcontainers::ContainerAsync<GenericImage>, SftpProvider) {
    let container = GenericImage::new("atmoz/sftp", "alpine")
        .with_exposed_port(22.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server listening on"))
        .with_cmd([format!("{USER}:{PASS}:::{}", BASE.trim_start_matches('/'))])
        .start()
        .await
        .expect("start atmoz/sftp container");
    let port = container
        .get_host_port_ipv4(22.tcp())
        .await
        .expect("mapped port");
    let provider = connect_provider(port).await;
    (container, provider)
}

fn vp(p: &str) -> VPath {
    VPath::parse(&format!("sftp://127.0.0.1:22{p}")).expect("valid wire")
}

/// Basic roundtrip against real OpenSSH: write, stat, list, read.
#[tokio::test]
async fn openssh_write_stat_list_read() {
    let (_c, p) = setup().await;

    let mut sink = p.write(&vp("/hello.txt")).await.expect("write opens");
    sink.write(Bytes::from_static(b"real content"))
        .await
        .unwrap();
    sink.commit().await.expect("commit");

    let e = p.stat(&vp("/hello.txt")).await.expect("stat");
    assert_eq!(e.kind, EntryKind::File);
    assert_eq!(e.size, Some(12));

    let mut stream = p.list(&vp("/")).await.expect("list opens");
    let mut seen = false;
    while let Some(item) = stream.next().await {
        let entry = item.expect("valid entry");
        if entry.path.file_name().map(|n| n.as_bytes()) == Some(b"hello.txt".as_slice()) {
            seen = true;
        }
    }
    assert!(seen, "the written file appears in the listing");

    let mut rd = p.read(&vp("/hello.txt"), None).await.expect("read");
    let mut out = Vec::new();
    while let Some(c) = rd.next().await {
        out.extend_from_slice(&c.unwrap());
    }
    assert_eq!(out, b"real content");
}

/// Resume by append (ADR 0012) against real OpenSSH: keep preserves the
/// partial, a second open resumes from the offset, commit concatenates.
#[tokio::test]
async fn openssh_resume_by_append() {
    let (_c, p) = setup().await;

    let (mut sink, already) = p.open_resumable(&vp("/big.bin")).await.expect("open 1");
    assert_eq!(already, 0);
    sink.write(Bytes::from_static(b"hello")).await.unwrap();
    sink.keep().await.expect("keep preserves the partial");

    let (mut sink, already) = p.open_resumable(&vp("/big.bin")).await.expect("open 2");
    assert_eq!(already, 5, "resumes from what was preserved");
    sink.write(Bytes::from_static(b"world")).await.unwrap();
    sink.commit().await.expect("commit");

    let mut rd = p.read(&vp("/big.bin"), None).await.expect("read");
    let mut out = Vec::new();
    while let Some(c) = rd.next().await {
        out.extend_from_slice(&c.unwrap());
    }
    assert_eq!(out, b"helloworld");
}
