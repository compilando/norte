//! Wiring for the FTP-by-plugin provider (#30 stage 3c, ADR 0033): replaces
//! the defunct `norte-vfs-ftp`. The host resolves the hostname to an IP (the
//! guest has no DNS), grants the `net` capability SCOPED to that IP (all
//! ports — passive FTP negotiates dynamic data ports), instantiates the
//! `ftp-provider` guest from the EMBEDDED `.wasm`, and calls `configure`.
//!
//! The guest establishes the connection itself (suppaftp sync over
//! wasi:sockets): the host cannot inject a live stream into it. Credentials
//! cross into the guest's memory for the `login`; without TLS (FTPS = debt,
//! aws-lc-rs does not compile to wasm) they already travel in the clear over
//! the wire.
//!
//! **`[config]` (P2 Task 4a) — documented deferral:** this provider does NOT
//! declare `[config]`, nor would it receive one if it did —
//! `connect_ftp_plugin` never calls
//! [`crate::plugin_provider::PluginProvider::set_settings`] (it stays at its
//! empty default, like any plugin with no `[config]`). Structural reason: the
//! FTP connection is born from `ConnectionSpec`/`connections.toml` (endpoint,
//! credentials, auth), NOT from the plugin catalog — there is no
//! `plugin.toml` declaring a `[config]` schema to resolve. See the
//! `plugin_provider` module doc for the deferral's detail.

use std::net::IpAddr;

use norte_plugin_host::{Capabilities as HostCaps, PluginRuntime};
use norte_vfs::proto::Error;

use crate::plugin_provider::{PluginProvider, map_runtime_error};

/// The FTP guest's `.wasm`, EMBEDDED (ADR 0033): it cannot be a build
/// dependency of `norte-core` (the `wasm32-wasip2` target may be missing on
/// the build host). Regenerated with `just build-ftp-wasm`.
const FTP_PROVIDER_WASM: &[u8] = include_bytes!("../resources/ftp-provider.wasm");

/// IP ranges that must NEVER be reachable from a guest (anti-SSRF): loopback
/// (unless the destination host is explicitly localhost — see
/// `connect_ftp_plugin`), link-local (`169.254.0.0/16`, `fe80::/10`), and the
/// cloud metadata range (`169.254.169.254` falls under link-local). Filtered
/// on the HOST after DNS resolution, before granting `net` (the guest
/// resolves by IP; without this filter a malicious hostname resolving to
/// metadata would be reachable).
fn is_forbidden_ip(ip: IpAddr, allow_loopback: bool) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            (v4.is_loopback() && !allow_loopback)
                || v4.is_link_local()
                || v4.is_unspecified()
                // Cloud metadata range (169.254.169.254) is already
                // link-local; covered above. Multicast/broadcast are not TCP
                // destinations.
                || v4.is_multicast()
                || v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            // IPv4-mapped (`::ffff:a.b.c.d`): on a dual-stack host the
            // kernel routes the connect to the embedded IPv4, so
            // `::ffff:169.254.169.254` would reach metadata by slipping
            // past the IPv6 checks (security HIGH). Canonicalized to the
            // IPv4 and evaluated with the v4 rules — closes mapped
            // loopback/link-local/RFC1918. Only `to_ipv4_mapped` (not
            // `to_ipv4`): the latter also converts `::1`→`0.0.0.1`, which
            // would stop looking like loopback; the IPv4-compat addresses
            // (deprecated, not routed to v4) are covered by the v6 checks
            // below.
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_forbidden_ip(IpAddr::V4(mapped), allow_loopback);
            }
            (v6.is_loopback() && !allow_loopback)
                || v6.is_unspecified()
                || v6.is_multicast()
                // Link-local unicast fe80::/10 (no stable helper in std).
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Resolves `host` to an IP (the guest connects by IP; it has no DNS) and
/// filters it against the forbidden ranges (SSRF). `allow_loopback` lets
/// `127.0.0.1`/`::1` through only when the destination host is explicitly
/// loopback (tests, local tunnels) — never for a hostname that surprisingly
/// resolves to loopback.
pub(crate) fn resolve_ip(host: &str, port: u16) -> Result<IpAddr, Error> {
    use std::net::ToSocketAddrs;
    // If the user literally asked for loopback, the loopback destination is allowed.
    let allow_loopback = host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
        || host.eq_ignore_ascii_case("localhost");
    let ip = (host, port)
        .to_socket_addrs()
        .map_err(|_| Error::ProviderUnavailable { retryable: true })?
        .map(|sa| sa.ip())
        .find(|ip| !is_forbidden_ip(*ip, allow_loopback))
        .ok_or(Error::ProviderUnavailable { retryable: true })?;
    Ok(ip)
}

/// Builds a CONNECTED FTP [`PluginProvider`]: resolves DNS host-side (with
/// the anti-SSRF filter), grants `net` to the resolved IP, instantiates the
/// embedded guest, and configures the session (connect + login + binary/MLSD
/// mode).
///
/// # Errors
/// [`Error::ProviderUnavailable`] if resolution fails or the IP is
/// forbidden; [`Error::Internal`] if instantiating the guest traps; the
/// guest's own logical error if `configure` fails (login rejected, server
/// down).
pub async fn connect_ftp_plugin(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    base: &str,
) -> Result<PluginProvider, Error> {
    let host_owned = host.to_string();
    let ip = crate::blocking::spawn_blocking(move || resolve_ip(&host_owned, port))
        .await
        .map_err(|_| Error::Internal { panic: true })??;
    let endpoint = format!("{}:{port}", fmt_ip(ip));
    // net: bare-ip = all ports (control + dynamic passive data).
    let caps = HostCaps::with_net(vec![ip.to_string()]);

    // Instantiating the component (compiles cranelift) is blocking →
    // spawn_blocking (rule 2). The PluginProvider is built from the
    // EMBEDDED bytes.
    let provider = crate::blocking::spawn_blocking(move || {
        let runtime = PluginRuntime::new().map_err(|e| map_runtime_error(&e))?;
        PluginProvider::from_bytes(runtime, FTP_PROVIDER_WASM, caps, "ftp")
            .map_err(|e| map_runtime_error(&e))
    })
    .await
    .map_err(|_| Error::Internal { panic: true })??;

    provider
        .configure(
            endpoint,
            user.to_string(),
            password.to_string(),
            base.to_string(),
        )
        .await?;
    Ok(provider)
}

/// Formats an IP for the `ip:port` endpoint. IPv6 goes in brackets
/// (`[::1]:21`) so the guest's `suppaftp`/`ToSocketAddrs` can parse it.
pub(crate) fn fmt_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn forbidden_ips_are_rejected() {
        // Link-local / cloud metadata.
        assert!(is_forbidden_ip(
            IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
            false
        ));
        assert!(is_forbidden_ip(
            IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1)),
            false
        ));
        // Loopback forbidden unless explicitly allowed.
        assert!(is_forbidden_ip(IpAddr::V4(Ipv4Addr::LOCALHOST), false));
        assert!(!is_forbidden_ip(IpAddr::V4(Ipv4Addr::LOCALHOST), true));
        // IPv6 link-local fe80::/10 and loopback.
        assert!(is_forbidden_ip(
            IpAddr::V6("fe80::1".parse().unwrap()),
            false
        ));
        assert!(is_forbidden_ip(IpAddr::V6(Ipv6Addr::LOCALHOST), false));
        assert!(!is_forbidden_ip(IpAddr::V6(Ipv6Addr::LOCALHOST), true));
        // A normal public IP passes.
        assert!(!is_forbidden_ip(
            IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
            false
        ));
    }

    #[test]
    fn ipv4_mapped_ipv6_does_not_bypass_the_denylist() {
        // security HIGH: `::ffff:169.254.169.254` (cloud metadata) must NOT
        // slip through as "not forbidden" IPv6 — it's canonicalized to IPv4.
        let metadata: IpAddr = "::ffff:169.254.169.254".parse().unwrap();
        assert!(is_forbidden_ip(metadata, false), "mapped metadata blocked");
        let mapped_loop: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(
            is_forbidden_ip(mapped_loop, false),
            "mapped loopback blocked"
        );
        assert!(
            !is_forbidden_ip(mapped_loop, true),
            "mapped loopback allowed with the explicit flag"
        );
        // `::1` still looks like v6 loopback (to_ipv4_mapped does not touch it).
        assert!(is_forbidden_ip("::1".parse().unwrap(), false));
        // A mapped public IPv4 passes, same as its direct v4 form.
        let mapped_pub: IpAddr = "::ffff:93.184.216.34".parse().unwrap();
        assert!(!is_forbidden_ip(mapped_pub, false));
    }

    #[test]
    fn ipv6_endpoint_is_bracketed() {
        assert_eq!(fmt_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)), "[::1]");
        assert_eq!(fmt_ip(IpAddr::V4(Ipv4Addr::LOCALHOST)), "127.0.0.1");
    }

    #[test]
    fn localhost_resolves_and_is_allowed() {
        let ip = resolve_ip("127.0.0.1", 21).expect("literal loopback allowed");
        assert!(ip.is_loopback());
    }
}
