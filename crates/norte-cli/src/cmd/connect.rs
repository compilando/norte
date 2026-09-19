//! `norte connect` (fase 6e) y la resolución de rutas/URLs compartida por el
//! resto de subcomandos: TOFU, schemes remotos y `VPath` desde un `PathBuf`.

use std::process::ExitCode;

use anyhow::Context;
use norte_core::Engine;
use norte_core::backend::Backend;
use norte_proto::VPath;

/// Schemes remotos que la CLI enruta por URL. ALLOWLIST explícita: un path
/// local puede llamarse legalmente `a://b` (o `./x://y`) y debe seguir
/// siendo un fichero — solo lo que empieza EXACTAMENTE por estos prefijos se
/// trata como URL remota.
const REMOTE_SCHEMES: [&str; 3] = ["sftp://", "ftp://", "s3://"];

/// ¿El arg es una URL de archivo-como-directorio (ADR 0018)? Exige la forma
/// completa `<formato>+<scheme>://…` con formato de la whitelist de proto
/// (la reserva normativa garantiza que ningún provider legítimo empieza
/// así, test abajo): un path local raro tipo `zip+dir/sub://y` sigue
/// siendo nativo.
///
/// Delegado en `norte_proto::scheme_archive_format` (longest-match, #55) en
/// vez de reimplementar la gramática con `split_once('+')`: un token puede
/// contener `+` propio (`tar+gz`), y duplicar la whitelist aquí divergiría
/// en cuanto proto gane un formato compuesto nuevo.
fn is_archive_url(s: &str) -> bool {
    let Some((scheme, _)) = s.split_once("://") else {
        return false;
    };
    let Some(fmt) = norte_proto::scheme_archive_format(scheme) else {
        return false;
    };
    let inner = &scheme[fmt.len() + 1..];
    !inner.is_empty() && !inner.contains('/')
}

/// #95: el daemon honra `[archive]` de norte.toml (capa usuario) — antes
/// servía con los defaults compilados y ni operador ni policy podían bajar
/// los límites anti-bomba para agentes. Fail-loud: un norte.toml roto
/// aborta el arranque (mismo criterio que policy.toml).
#[cfg(unix)]
pub(crate) async fn apply_archive_limits(engine: &Engine) -> anyhow::Result<()> {
    if let Some(limits) =
        tokio::task::spawn_blocking(norte_core::archive_config::load_archive_limits)
            .await
            .context("carga de norte.toml")?
            .context("norte.toml inválido ([archive])")?
    {
        engine.set_archive_limits(limits);
    }
    // Ítem 11 del roadmap: qué programa lee los RAR. `None` = sondear PATH.
    engine.set_rar_delegate(
        tokio::task::spawn_blocking(norte_core::archive_config::load_rar_delegate)
            .await
            .context("carga de norte.toml")?
            .context("norte.toml inválido ([archive] rar_delegate)")?,
    );
    Ok(())
}

/// ¿`s` es una URL que la CLI enruta como remota? Los schemes del core, los
/// de archivo-como-directorio, y los que declare un provider plugin
/// INSTALADO (`plugin_schemes`): en cuanto hay quien sirve `webdav://`, un
/// argumento `webdav://x` deja de ser un fichero local con nombre raro.
/// Consentido o no — enrutar no concede nada; conectar sigue fail-closed.
fn is_remote_url(s: &str, plugin_schemes: &[String]) -> bool {
    REMOTE_SCHEMES.iter().any(|p| s.starts_with(p))
        || is_archive_url(s)
        || plugin_schemes.iter().any(|sch| {
            s.strip_prefix(sch.as_str())
                .is_some_and(|resto| resto.len() > 3 && resto.starts_with("://"))
        })
}

/// Los schemes de los provider plugins instalados bajo el config dir de
/// este proceso: un `plugin.toml` por plugin, leído UNA vez por proceso y
/// solo para enrutar (`cp` pregunta dos veces por comando).
fn plugin_schemes() -> &'static [String] {
    static SCHEMES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    SCHEMES.get_or_init(|| {
        norte_core::plugins::installed_provider_schemes(&norte_core::connect::config_dir())
    })
}

pub(crate) fn vpath(path: &std::path::Path) -> anyhow::Result<VPath> {
    // Una URL remota va por el parser wire; todo lo demás es un path NATIVO
    // local (bytes, jamás forzados a UTF-8 — un arg no-UTF8 no puede ser URL
    // y cae al camino nativo).
    if let Some(s) = path.to_str()
        && is_remote_url(s, plugin_schemes())
    {
        reject_inline_password(s)?;
        return VPath::parse(s).with_context(|| norte_i18n::ta("cli-invalid-url", &[("url", s)]));
    }
    norte_vfs_local::vpath_from_native(path)
        .with_context(|| format!("path no representable: {}", path.display()))
}

/// Rechaza `user:pass@host` en una URL ANTES de que entre a `VPath::parse`
/// (que la aceptaría) y por tanto a spans/errores: mensaje ESTÁTICO, sin
/// ecoar la URL (regla 10). El parser de conexiones la rechazaría después,
/// pero para entonces ya habría tocado logs.
fn reject_inline_password(url: &str) -> anyhow::Result<()> {
    let after_scheme = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = after_scheme.split('/').next().unwrap_or(after_scheme);
    if let Some((userinfo, _)) = authority.rsplit_once('@')
        && userinfo.contains(':')
    {
        anyhow::bail!(norte_i18n::t("cli-inline-password"));
    }
    Ok(())
}

/// Flujo TOFU interactivo (ADR 0015 D/G): ante un `Error::HostKeyUnknown`
/// muestra la huella, pide confirmación por el terminal y, si el usuario
/// acepta, la registra (el core re-verifica anti-TOCTOU) y devuelve `true`
/// (reintentar la operación). Errores que no son TOFU → `false` (el caller
/// reporta el original). Sin terminal NO se confía nada: instrucciones y error.
pub(crate) async fn tofu_confirm(
    backend: &Backend,
    err: &norte_proto::Error,
) -> anyhow::Result<bool> {
    use std::io::IsTerminal;
    let norte_proto::Error::HostKeyUnknown {
        host,
        port,
        algo,
        fingerprint,
    } = err
    else {
        return Ok(false);
    };
    let port_shown = port.unwrap_or(22).to_string();
    eprintln!(
        "{}",
        norte_i18n::ta(
            "cli-hostkey-unknown",
            &[("host", host.as_str()), ("port", port_shown.as_str())]
        )
    );
    eprintln!(
        "{}",
        norte_i18n::ta(
            "cli-hostkey-fingerprint",
            &[
                ("algo", algo.as_str()),
                ("fingerprint", fingerprint.as_str())
            ]
        )
    );
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(norte_i18n::t("cli-hostkey-noninteractive"));
    }
    eprint!("{} ", norte_i18n::t("cli-hostkey-prompt"));
    // stdin es bloqueante: fuera del reactor (regla 2).
    let line = tokio::task::spawn_blocking(|| {
        let mut s = String::new();
        std::io::stdin().read_line(&mut s).map(|_| s)
    })
    .await
    .context(norte_i18n::t("cli-confirm-read"))??;
    let yes = matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "s" | "si" | "sí" | "y" | "yes"
    );
    if !yes {
        anyhow::bail!(norte_i18n::t("cli-hostkey-refused"));
    }
    backend
        .trust_host_key(host, *port, algo, fingerprint)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    eprintln!("{}", norte_i18n::t("cli-hostkey-trusted"));
    Ok(true)
}

/// `norte connect <nombre|url>`: establece la conexión (disparando el flujo
/// TOFU si es el primer contacto) y confirma. El valor duradero es el
/// registro de la host key + la validación de credenciales.
pub(crate) async fn connect_cmd(
    backend: &Backend,
    target: &str,
    daemon: bool,
) -> anyhow::Result<ExitCode> {
    if daemon {
        // La resolución por nombre lee el config LOCAL; contra un daemon
        // remoto la semántica cambia — se difiere (mínimo viable, ADR 0015 G).
        anyhow::bail!(norte_i18n::t("cli-connect-daemon-unsupported"));
    }
    let url = if target.contains("://") {
        target.to_string()
    } else {
        // Nombre de connections.toml → su URL (el core la resuelve).
        norte_core::connect::named_url(&norte_core::connect::config_dir(), target)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context(norte_i18n::t("cli-connect-failed"))?
    };
    reject_inline_password(&url)?;
    let root = VPath::parse(&url)
        .with_context(|| norte_i18n::ta("cli-invalid-url", &[("url", url.as_str())]))?;
    // #322: el canal del PORQUÉ, tomado ANTES del intento. Este es el comando
    // que se teclea justo para averiguar por qué una conexión no entra, y
    // hasta ahora contestaba «permiso denegado» a un secreto vacío, a una
    // clave equivocada y a un bucket ajeno por igual. Se toma aquí y no en el
    // arranque porque ningún otro subcomando lo mira.
    let mut fallos = backend.take_failed();
    // capabilities fuerza el establecimiento por el camino normal del engine.
    let result = match backend.capabilities(&root).await {
        Err(e) if tofu_confirm(backend, &e).await? => backend.capabilities(&root).await,
        other => other,
    };
    if let Err(e) = result {
        // El motivo, si el core supo contarlo. Va ANTES del error para que la
        // última línea siga siendo la categoría, que es lo que un script mira.
        if let Some(f) = fallos.as_mut().and_then(|rx| rx.try_recv().ok()) {
            eprintln!(
                "{}",
                norte_frontend::banners::failure_line(norte_i18n::active(), &f)
            );
        }
        return Err(anyhow::anyhow!("{e}")).context(norte_i18n::t("cli-connect-failed"));
    }
    println!(
        "{}",
        norte_i18n::ta("cli-connect-ok", &[("target", target)])
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un provider plugin instalado añade su scheme al enrutado de la CLI; sin
    /// él, el mismo argumento sigue siendo un fichero local (`a://b` es un
    /// nombre legal). El scheme casa ENTERO: `mem` instalado no convierte
    /// `memplug://x` en URL.
    #[test]
    fn un_scheme_de_plugin_instalado_enruta_como_url() {
        let ninguno: Vec<String> = vec![];
        assert!(!is_remote_url("memplug://host", &ninguno));
        let memplug = vec!["memplug".to_string()];
        assert!(is_remote_url("memplug://host", &memplug));
        assert!(
            !is_remote_url("memplug://", &memplug),
            "sin authority no es URL"
        );
        let mem = vec!["mem".to_string()];
        assert!(
            !is_remote_url("memplug://host", &mem),
            "prefijo no es scheme"
        );
        // Los del core y los de archivo siguen entrando sin plugin.
        assert!(is_remote_url("sftp://h", &ninguno));
        assert!(is_remote_url("zip+file:///a.zip/!/x", &ninguno));
    }

    /// Reserva normativa de ADR 0018: ningún scheme remoto de la allowlist
    /// puede empezar por `<formato>+` — el registro de formatos manda.
    #[test]
    fn remote_schemes_respetan_la_reserva_de_formatos() {
        for scheme in REMOTE_SCHEMES {
            for format in norte_proto::ARCHIVE_FORMATS {
                assert!(
                    !scheme.starts_with(&format!("{format}+")),
                    "{scheme} invade el namespace del formato {format}"
                );
            }
        }
    }

    #[test]
    fn urls_de_archivo_van_por_el_parser_wire() {
        let p = vpath(std::path::Path::new("zip+file:///tmp/a.zip/!/x")).expect("parsea");
        assert_eq!(p.scheme(), "zip+file");
        // Un path local que solo se PARECE (sin `://`) sigue siendo nativo.
        let p = vpath(std::path::Path::new("zip+file")).expect("nativo");
        assert_eq!(p.scheme(), "file");
        // Password inline en un compuesto remoto: mismo guard que siempre.
        // Directo contra el guard (el parse TAMBIÉN lo rechaza desde #46,
        // pero este test protege la defensa en profundidad de la CLI).
        assert!(reject_inline_password("tar+sftp://u:pass@h/a.tar/!").is_err());
        assert!(vpath(std::path::Path::new("tar+sftp://u:pass@h/a.tar/!")).is_err());
        // Paths locales patológicos que se PARECEN: nativos, no URL.
        for nativo in ["zip+dir/sub://y", "tar+xz", "zip+://x"] {
            assert!(!is_archive_url(nativo), "{nativo} debe ser nativo");
        }
    }

    /// #55: `tar+gz` es un TOKEN COMPUESTO en la whitelist de proto —
    /// `is_archive_url` debe reconocerlo vía `scheme_archive_format`
    /// (longest-match), no reimplementando la gramática con `split_once('+')`
    /// (eso dejaría un interior huérfano tipo `gz+file` para casos con más de
    /// un nivel, y duplica una whitelist que ya vive en proto — regla 8).
    #[test]
    fn is_archive_url_reconoce_targz_compuesto() {
        assert!(is_archive_url("tar+gz+file://x"));
        assert!(is_archive_url("tar+gz+sftp://h/a.tgz/!/x"));
        // #56: anidado multi-capa también enruta por el parser wire.
        assert!(is_archive_url("zip+tar+file:///b.tar/!/i.zip/!/f"));
        // El wire completo enruta por el parser y compone el scheme real.
        let p = vpath(std::path::Path::new("tar+gz+file:///a.tgz/!/x")).expect("parsea");
        assert_eq!(p.scheme(), "tar+gz+file");
    }
}
