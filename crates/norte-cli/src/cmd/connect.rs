//! `norte connect` (phase 6e) and the path/URL resolution shared by the
//! rest of the subcommands: TOFU, remote schemes and `VPath` from a
//! `PathBuf`.

use std::process::ExitCode;

use anyhow::Context;
use norte_core::backend::Backend;
use norte_proto::VPath;

/// Remote schemes the CLI routes by URL. Explicit ALLOWLIST: a local path
/// can legally be called `a://b` (or `./x://y`) and must remain a file —
/// only what starts EXACTLY with these prefixes is treated as a remote
/// URL.
const REMOTE_SCHEMES: [&str; 3] = ["sftp://", "ftp://", "s3://"];

/// Is the arg an archive-as-directory URL (ADR 0018)? Requires the full
/// `<format>+<scheme>://…` shape with a format from proto's whitelist
/// (the normative reservation guarantees no legitimate provider starts
/// this way, test below): an odd local path like `zip+dir/sub://y`
/// remains native.
///
/// Delegated to `norte_proto::scheme_archive_format` (longest-match, #55)
/// instead of reimplementing the grammar with `split_once('+')`: a token
/// can contain its own `+` (`tar+gz`), and duplicating the whitelist here
/// would diverge as soon as proto gains a new compound format.
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

/// Is `s` a URL the CLI routes as remote? The core's schemes, the
/// archive-as-directory ones, and whatever an INSTALLED provider plugin
/// declares (`plugin_schemes`): as soon as someone serves `webdav://`, a
/// `webdav://x` argument stops being a local file with an odd name.
/// Whether consented or not — routing grants nothing; connecting stays
/// fail-closed.
fn is_remote_url(s: &str, plugin_schemes: &[String]) -> bool {
    REMOTE_SCHEMES.iter().any(|p| s.starts_with(p))
        || is_archive_url(s)
        || plugin_schemes.iter().any(|sch| {
            s.strip_prefix(sch.as_str())
                .is_some_and(|rest| rest.len() > 3 && rest.starts_with("://"))
        })
}

/// The schemes of the provider plugins installed under this process's
/// config dir: one `plugin.toml` per plugin, read ONCE per process and
/// only to route (`cp` asks twice per command).
fn plugin_schemes() -> &'static [String] {
    static SCHEMES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    SCHEMES.get_or_init(|| {
        norte_core::plugins::installed_provider_schemes(&norte_core::connect::config_dir())
    })
}

/// A command-line argument, as a `VPath`.
///
/// Stays in the CLI on purpose, and not in `norte-frontend` next to
/// `goto::looks_path`: they are TWO grammars for two different inputs.
/// The TUI's and the window's "go to" rejects a relative path (where it
/// leads cannot depend on the pane) and takes any `x://` as a URL; a
/// shell argument is almost always relative to the cwd, and `a://b` has
/// to remain a local file unless its scheme is in the allowlist
/// ([`REMOTE_SCHEMES`], the archive ones and the installed plugins'). What
/// IS the core's job — rejecting a `user:pass@`, knowing whether a scheme
/// has someone serving it — is already done by `VPath::parse` and the
/// connector; here it is only decided whether an argument is a URL or a
/// path.
pub(crate) fn vpath(path: &std::path::Path) -> anyhow::Result<VPath> {
    // A remote URL goes through the wire parser; everything else is a
    // NATIVE local path (bytes, never forced to UTF-8 — a non-UTF-8 arg
    // cannot be a URL and falls to the native path).
    if let Some(s) = path.to_str()
        && is_remote_url(s, plugin_schemes())
    {
        reject_inline_password(s)?;
        return VPath::parse(s).with_context(|| norte_i18n::ta("cli-invalid-url", &[("url", s)]));
    }
    norte_vfs_local::vpath_from_native(path)
        .with_context(|| format!("unrepresentable path: {}", path.display()))
}

/// Rejects `user:pass@host` in a URL BEFORE it enters `VPath::parse`
/// (which would accept it) and therefore spans/errors: a STATIC message,
/// without echoing the URL (hard rule 10). The connections parser would
/// reject it afterward, but by then it would already have touched logs.
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

/// Interactive TOFU flow (ADR 0015 D/G): faced with an
/// `Error::HostKeyUnknown` it shows the fingerprint, asks for confirmation
/// via the terminal and, if the user accepts, records it (the core
/// re-verifies against TOCTOU) and returns `true` (retry the operation).
/// Errors that are not TOFU → `false` (the caller reports the original).
/// With no terminal NOTHING is trusted: instructions and error.
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
    // stdin is blocking: off the reactor (hard rule 2).
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

/// `norte connect <name|url>`: establishes the connection (triggering the
/// TOFU flow if it is the first contact) and confirms. The lasting value
/// is the host key record + the credential validation.
pub(crate) async fn connect_cmd(
    backend: &Backend,
    target: &str,
    daemon: bool,
) -> anyhow::Result<ExitCode> {
    if daemon {
        // Resolution by name reads the LOCAL config; against a remote
        // daemon the semantics change — deferred (minimum viable, ADR
        // 0015 G).
        anyhow::bail!(norte_i18n::t("cli-connect-daemon-unsupported"));
    }
    let url = if target.contains("://") {
        target.to_string()
    } else {
        // A connections.toml name → its URL (the core resolves it).
        norte_core::connect::named_url(&norte_core::connect::config_dir(), target)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context(norte_i18n::t("cli-connect-failed"))?
    };
    reject_inline_password(&url)?;
    let root = VPath::parse(&url)
        .with_context(|| norte_i18n::ta("cli-invalid-url", &[("url", url.as_str())]))?;
    // #322: the WHY channel, taken BEFORE the attempt. This is the command
    // typed exactly to find out why a connection does not go through, and
    // until now it answered "permission denied" for an empty secret, a
    // wrong key and someone else's bucket alike. It is taken here and not
    // at startup because no other subcommand looks at it.
    let mut failures = backend.take_failed();
    // capabilities forces establishment through the engine's normal path.
    let result = match backend.capabilities(&root).await {
        Err(e) if tofu_confirm(backend, &e).await? => backend.capabilities(&root).await,
        other => other,
    };
    if let Err(e) = result {
        // The reason, if the core managed to tell it. Goes BEFORE the
        // error so the last line stays the category, which is what a
        // script looks at.
        if let Some(f) = failures.as_mut().and_then(|rx| rx.try_recv().ok()) {
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

    /// An installed provider plugin adds its scheme to the CLI's routing;
    /// without it, the same argument stays a local file (`a://b` is a
    /// legal name). The scheme must match WHOLE: an installed `mem` does
    /// not turn `memplug://x` into a URL.
    #[test]
    fn an_installed_plugin_scheme_routes_as_a_url() {
        let none: Vec<String> = vec![];
        assert!(!is_remote_url("memplug://host", &none));
        let memplug = vec!["memplug".to_string()];
        assert!(is_remote_url("memplug://host", &memplug));
        assert!(
            !is_remote_url("memplug://", &memplug),
            "no authority is not a URL"
        );
        let mem = vec!["mem".to_string()];
        assert!(
            !is_remote_url("memplug://host", &mem),
            "a prefix is not a scheme"
        );
        // The core's and the archive ones still get in without a plugin.
        assert!(is_remote_url("sftp://h", &none));
        assert!(is_remote_url("zip+file:///a.zip/!/x", &none));
    }

    /// ADR 0018's normative reservation: no remote scheme in the allowlist
    /// can start with `<format>+` — the format registry rules.
    #[test]
    fn remote_schemes_respect_the_format_reservation() {
        for scheme in REMOTE_SCHEMES {
            for format in norte_proto::ARCHIVE_FORMATS {
                assert!(
                    !scheme.starts_with(&format!("{format}+")),
                    "{scheme} invades format {format}'s namespace"
                );
            }
        }
    }

    #[test]
    fn archive_urls_go_through_the_wire_parser() {
        let p = vpath(std::path::Path::new("zip+file:///tmp/a.zip/!/x")).expect("parses");
        assert_eq!(p.scheme(), "zip+file");
        // A local path that only LOOKS like one (no `://`) stays native.
        let p = vpath(std::path::Path::new("zip+file")).expect("native");
        assert_eq!(p.scheme(), "file");
        // Inline password in a remote compound: same guard as always.
        // Directly against the guard (the parse ALSO rejects it since
        // #46, but this test protects the CLI's defense in depth).
        assert!(reject_inline_password("tar+sftp://u:pass@h/a.tar/!").is_err());
        assert!(vpath(std::path::Path::new("tar+sftp://u:pass@h/a.tar/!")).is_err());
        // Pathological local paths that LOOK like one: native, not URL.
        for native in ["zip+dir/sub://y", "tar+xz", "zip+://x"] {
            assert!(!is_archive_url(native), "{native} must be native");
        }
    }

    /// #55: `tar+gz` is a COMPOUND TOKEN in proto's whitelist —
    /// `is_archive_url` must recognize it via `scheme_archive_format`
    /// (longest-match), not by reimplementing the grammar with
    /// `split_once('+')` (that would leave an orphaned interior like
    /// `gz+file` for cases with more than one level, and duplicates a
    /// whitelist that already lives in proto — rule 8).
    #[test]
    fn is_archive_url_recognizes_compound_targz() {
        assert!(is_archive_url("tar+gz+file://x"));
        assert!(is_archive_url("tar+gz+sftp://h/a.tgz/!/x"));
        // #56: multi-layer nesting also routes through the wire parser.
        assert!(is_archive_url("zip+tar+file:///b.tar/!/i.zip/!/f"));
        // The whole wire routes through the parser and composes the real
        // scheme.
        let p = vpath(std::path::Path::new("tar+gz+file:///a.tgz/!/x")).expect("parses");
        assert_eq!(p.scheme(), "tar+gz+file");
    }
}
