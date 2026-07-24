//! Wiring del provider FTP-por-plugin (#30 stage 3c, ADR 0033): reemplaza al
//! difunto `norte-vfs-ftp`. El host resuelve el hostname a IP (el guest no tiene
//! DNS), concede la capability `net` ACOTADA a esa IP (todos los puertos — el
//! FTP pasivo negocia puertos de datos dinámicos), instancia el guest
//! `ftp-provider` desde el `.wasm` EMBEBIDO y llama a `configure`.
//!
//! El guest establece la conexión él mismo (suppaftp sync sobre wasi:sockets):
//! el host no puede inyectarle un stream vivo. Las credenciales cruzan a memoria
//! del guest para el `login`; sin TLS (FTPS = deuda, aws-lc-rs no compila a
//! wasm) ya viajan en claro por el cable.
//!
//! **`[config]` (P2 Task 4a) — deferral documentado:** este provider NO
//! declara `[config]` ni lo recibiría si lo hiciera — `connect_ftp_plugin`
//! nunca llama a [`crate::plugin_provider::PluginProvider::set_settings`]
//! (queda en su default vacío, como cualquier plugin sin `[config]`). Motivo
//! estructural: la conexión FTP nace de `ConnectionSpec`/`connections.toml`
//! (endpoint, credenciales, auth), NO del catálogo de plugins — no hay
//! `plugin.toml` que declare un esquema `[config]` que resolver. Ver el doc
//! del módulo `plugin_provider` para el detalle del deferral.

use std::net::IpAddr;

use norte_plugin_host::{Capabilities as HostCaps, PluginRuntime};
use norte_vfs::proto::Error;

use crate::plugin_provider::{PluginProvider, map_runtime_error};

/// El `.wasm` del guest FTP, EMBEBIDO (ADR 0033): no puede ser dependencia de
/// build de `norte-core` (el target `wasm32-wasip2` puede faltar en el host de
/// compilación). Se regenera con `just build-ftp-wasm`.
const FTP_PROVIDER_WASM: &[u8] = include_bytes!("../resources/ftp-provider.wasm");

/// Rangos de IP que JAMÁS deben alcanzarse desde un guest (anti-SSRF): loopback
/// (salvo destino localhost explícito — ver `connect_ftp_plugin`), link-local
/// (`169.254.0.0/16`, `fe80::/10`) y el rango de metadata de nube
/// (`169.254.169.254` cae en link-local). Se filtra en el HOST tras resolver
/// DNS, antes de conceder `net` (el guest resuelve por IP; sin este filtro un
/// hostname malicioso que resuelva a metadata sería alcanzable).
fn is_forbidden_ip(ip: IpAddr, allow_loopback: bool) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            (v4.is_loopback() && !allow_loopback)
                || v4.is_link_local()
                || v4.is_unspecified()
                // Rango de metadata de nube (169.254.169.254) ya es link-local;
                // se cubre arriba. Multicast/broadcast no son destinos TCP.
                || v4.is_multicast()
                || v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            // IPv4-mapped (`::ffff:a.b.c.d`): en un host dual-stack el kernel
            // enruta el connect a la IPv4 embebida, así que
            // `::ffff:169.254.169.254` alcanzaría la metadata saltándose los
            // checks IPv6 (security HIGH). Se canonicaliza a la IPv4 y se evalúa
            // con las reglas v4 — cierra loopback/link-local/RFC1918 mapeados.
            // Solo `to_ipv4_mapped` (no `to_ipv4`): esta última también convierte
            // `::1`→`0.0.0.1`, que dejaría de verse como loopback; los IPv4-compat
            // (deprecados, no enrutados a v4) los cubren los checks v6 de abajo.
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_forbidden_ip(IpAddr::V4(mapped), allow_loopback);
            }
            (v6.is_loopback() && !allow_loopback)
                || v6.is_unspecified()
                || v6.is_multicast()
                // Link-local unicast fe80::/10 (no hay helper estable en std).
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Resuelve `host` a una IP (el guest conecta por IP; no tiene DNS) y la filtra
/// contra los rangos prohibidos (SSRF). `allow_loopback` deja pasar
/// `127.0.0.1`/`::1` solo cuando el host destino es explícitamente loopback
/// (tests, túneles locales) — jamás por un hostname que resuelva sorpresivamente
/// a loopback.
fn resolve_ip(host: &str, port: u16) -> Result<IpAddr, Error> {
    use std::net::ToSocketAddrs;
    // Si el usuario pidió literalmente loopback, se permite el destino loopback.
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

/// Construye un [`PluginProvider`] FTP CONECTADO: resuelve DNS host-side (con
/// filtro anti-SSRF), concede `net` a la IP resuelta, instancia el guest
/// embebido y configura la sesión (connect + login + modo binario/MLSD).
///
/// # Errors
/// [`Error::ProviderUnavailable`] si la resolución falla o la IP está prohibida;
/// [`Error::Internal`] si la instanciación del guest atrapa; el error lógico del
/// guest si `configure` falla (login rechazado, servidor caído).
pub async fn connect_ftp_plugin(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    base: &str,
) -> Result<PluginProvider, Error> {
    let host_owned = host.to_string();
    let ip = tokio::task::spawn_blocking(move || resolve_ip(&host_owned, port))
        .await
        .map_err(|_| Error::Internal { panic: true })??;
    let endpoint = format!("{}:{port}", fmt_ip(ip));
    // net: bare-ip = todos los puertos (control + datos pasivos dinámicos).
    let caps = HostCaps::with_net(vec![ip.to_string()]);

    // Instanciar el componente (compila cranelift) es bloqueante → spawn_blocking
    // (regla 2). El PluginProvider se construye desde los BYTES embebidos.
    let provider = tokio::task::spawn_blocking(move || {
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

/// Formatea una IP para el endpoint `ip:puerto`. IPv6 va entre corchetes
/// (`[::1]:21`) para que `suppaftp`/`ToSocketAddrs` del guest lo parsee.
fn fmt_ip(ip: IpAddr) -> String {
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
        // Link-local / metadata de nube.
        assert!(is_forbidden_ip(
            IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
            false
        ));
        assert!(is_forbidden_ip(
            IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1)),
            false
        ));
        // Loopback prohibido salvo permiso explícito.
        assert!(is_forbidden_ip(IpAddr::V4(Ipv4Addr::LOCALHOST), false));
        assert!(!is_forbidden_ip(IpAddr::V4(Ipv4Addr::LOCALHOST), true));
        // IPv6 link-local fe80::/10 y loopback.
        assert!(is_forbidden_ip(
            IpAddr::V6("fe80::1".parse().unwrap()),
            false
        ));
        assert!(is_forbidden_ip(IpAddr::V6(Ipv6Addr::LOCALHOST), false));
        assert!(!is_forbidden_ip(IpAddr::V6(Ipv6Addr::LOCALHOST), true));
        // Una IP pública normal pasa.
        assert!(!is_forbidden_ip(
            IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
            false
        ));
    }

    #[test]
    fn ipv4_mapped_ipv6_does_not_bypass_the_denylist() {
        // security HIGH: `::ffff:169.254.169.254` (metadata de nube) NO debe
        // colarse como IPv6 "no prohibida" — se canonicaliza a la IPv4.
        let metadata: IpAddr = "::ffff:169.254.169.254".parse().unwrap();
        assert!(
            is_forbidden_ip(metadata, false),
            "metadata mapeada bloqueada"
        );
        let mapped_loop: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(
            is_forbidden_ip(mapped_loop, false),
            "loopback mapeado bloqueado"
        );
        assert!(
            !is_forbidden_ip(mapped_loop, true),
            "loopback mapeado permitido con el flag explícito"
        );
        // `::1` sigue viéndose como loopback v6 (to_ipv4_mapped no lo toca).
        assert!(is_forbidden_ip("::1".parse().unwrap(), false));
        // Una IPv4 pública mapeada pasa, igual que su forma v4 directa.
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
        let ip = resolve_ip("127.0.0.1", 21).expect("loopback literal permitido");
        assert!(ip.is_loopback());
    }
}
