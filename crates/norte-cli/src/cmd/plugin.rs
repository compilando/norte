//! `norte plugin run|install|uninstall|list` (M4-P4).

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use norte_core::backend::Backend;

use crate::PluginCmd;

/// `norte plugin run <id> <command> [arg]`: ejecuta un comando de un plugin YA
/// aprobado+activado por el humano y escribe su salida a STDOUT. Va por el
/// `Backend` elegido con los flags globales (`--daemon`/`--socket`), como el
/// resto de operaciones (regla 7).
pub(crate) async fn plugin_cmd(
    backend: &Backend,
    cmd: PluginCmd,
    socket: Option<PathBuf>,
) -> anyhow::Result<ExitCode> {
    match cmd {
        PluginCmd::Run { id, command, arg } => {
            match backend.plugin_run_command(&id, &command, &arg).await {
                Ok(output) => {
                    println!("{output}");
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    eprintln!(
                        "{}",
                        norte_i18n::ta("cli-plugin-run-failed", &[("error", &e.to_string())])
                    );
                    Ok(ExitCode::FAILURE)
                }
            }
        }
        // Copia local, SIN daemon: instalar es mover ficheros al directorio de
        // config, y hacerlo depender de un daemon vivo sería pedirle al usuario
        // que arranque el programa para poder instalarle algo.
        PluginCmd::Install { path, force } => {
            let dir = norte_core::connect::config_dir();
            // Copiar un directorio es I/O bloqueante (regla 2), como el
            // borrado de `uninstall` que va al lado.
            let informe = tokio::task::spawn_blocking(move || {
                norte_core::plugins::install(&dir, &path, force)
            })
            .await
            .context("instalando")?;
            match informe {
                Ok(rep) => {
                    // El nombre viene del manifiesto de un tercero: se pinta
                    // saneado, como en el gestor. Texto por Fluent (#319).
                    let (nombre, _) = norte_frontend::display_name(rep.name.as_bytes());
                    let key = if rep.replaced {
                        "cli-plugin-replaced"
                    } else {
                        "cli-plugin-installed"
                    };
                    println!(
                        "{}",
                        norte_i18n::ta(key, &[("id", &rep.id), ("name", &nombre)])
                    );
                    if rep.replaced {
                        println!("{}", norte_i18n::t("cli-plugin-replaced-consent"));
                    }
                    println!("{}", norte_i18n::t("cli-plugin-unapproved"));
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    eprintln!("{e}");
                    Ok(ExitCode::FAILURE)
                }
            }
        }
        PluginCmd::Uninstall { id } => plugin_uninstall(backend, socket, &id).await,
        // `plugin.list`, el mismo catálogo que pinta el gestor: id, categoría,
        // los DOS hechos (aprobado, activado) y las capabilities que aprobar
        // concedería. El nombre viene de un tercero: saneado, como en el
        // gestor. Los rotos se cuentan, no se listan — `norte doctor` los
        // explica uno a uno.
        PluginCmd::List => plugin_list(backend).await,
    }
}

/// `norte plugin uninstall` (ADR 0113). Con `--daemon`, POR el daemon
/// (`plugin.uninstall`): borra en SU directorio de configuración y lo olvida
/// en memoria. Sin `--daemon`, en el disco de este proceso, como `install`.
///
/// Por el daemon solo si se pide. El socket por defecto sale del usuario, no
/// de `NORTE_CONFIG_DIR`, y nada en `initialize` dice qué directorio sirve
/// el daemon: un CLI con otro directorio que desinstalase por el que escucha
/// borraría la extensión —y retiraría su aprobación— en el directorio de ESE
/// daemon. Sin `--daemon`, si hay uno aceptando, se avisa de que la seguirá
/// listando hasta reiniciarse.
async fn plugin_uninstall(
    backend: &Backend,
    socket: Option<PathBuf>,
    id: &str,
) -> anyhow::Result<ExitCode> {
    use norte_core::plugins::UninstallError as U;
    if !norte_core::is_valid_plugin_id(id) {
        return Ok(uninstall_fallido(&U::InvalidId));
    }
    match backend {
        #[cfg(unix)]
        Backend::Remote(r) => {
            // «¿Está?» contra el catálogo DEL DAEMON, que es el directorio que
            // se borra. Y aquí y no por su respuesta: rehúsa con un
            // `INVALID_PARAMS` sin taxonomía, que llega como un `Internal`.
            let lista = backend
                .plugins_list()
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let esta = lista.plugins.iter().any(|p| p.id == id)
                || lista
                    .errors
                    .iter()
                    .any(|e| e.dir_bytes.as_deref().unwrap_or(e.dir.as_bytes()) == id.as_bytes());
            if !esta {
                return Ok(uninstall_fallido(&U::NotInstalled(id.to_owned())));
            }
            match r.plugins_uninstall(id).await {
                Ok(res) => Ok(uninstall_hecho(id, res.was_approved)),
                Err(e) => {
                    eprintln!(
                        "{}",
                        norte_i18n::ta("cli-plugin-uninstall-io", &[("error", &e.to_string())])
                    );
                    Ok(ExitCode::FAILURE)
                }
            }
        }
        Backend::Embedded(_) => {
            let dir = norte_core::connect::config_dir();
            let owned = id.to_owned();
            let informe =
                tokio::task::spawn_blocking(move || norte_core::plugins::uninstall(&dir, &owned))
                    .await
                    .context("desinstalando")?;
            // El aviso también cuando NO estaba: la forma más probable de
            // llegar a `NotInstalled` es justo el caso de ADR 0113 —un CLI
            // con otro `NORTE_CONFIG_DIR` que el daemon—, y ahí «no está
            // instalada» a secas no dice que existe `--daemon`.
            let code = match informe {
                Ok(rep) => uninstall_hecho(&rep.id, rep.was_approved),
                Err(e) => uninstall_fallido(&e),
            };
            avisar_si_hay_daemon(socket).await;
            Ok(code)
        }
    }
}

/// Avisa, sin arrancarlo, si un daemon ACEPTA en `socket`: seguirá listando
/// lo que se acaba de borrar por detrás hasta reiniciarse.
#[cfg(unix)]
async fn avisar_si_hay_daemon(socket: Option<PathBuf>) {
    /// Lo que se espera a un daemon que acepta y no contesta: es un aviso, y
    /// no puede colgar un comando que antes no tocaba el socket.
    const PLAZO: std::time::Duration = std::time::Duration::from_secs(2);
    let socket = match socket {
        Some(s) => s,
        None => {
            match tokio::task::spawn_blocking(|| norte_core::daemon::default_socket_path(None))
                .await
            {
                Ok(s) => s,
                Err(_) => return,
            }
        }
    };
    let conectar = norte_core::backend::remote::RemoteBackend::connect(
        socket,
        None,
        norte_proto::methods::ClientInfo {
            name: "norte-cli".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
    );
    if matches!(tokio::time::timeout(PLAZO, conectar).await, Ok(Ok(_))) {
        eprintln!("{}", norte_i18n::t("cli-plugin-uninstall-daemon-stale"));
    }
}

/// Sin daemon por socket unix no hay registro que se quede viejo.
#[cfg(not(unix))]
async fn avisar_si_hay_daemon(_socket: Option<PathBuf>) {}

/// Lo que `plugin uninstall` dice cuando borró.
fn uninstall_hecho(id: &str, was_approved: bool) -> ExitCode {
    println!(
        "{}",
        norte_i18n::ta("cli-plugin-uninstalled", &[("id", id)])
    );
    if was_approved {
        println!("{}", norte_i18n::t("cli-plugin-uninstalled-consent"));
    }
    ExitCode::SUCCESS
}

/// Lo que `plugin uninstall` dice cuando no pudo. Texto al usuario por
/// Fluent (#319); el `Display` del error se queda para los logs.
fn uninstall_fallido(e: &norte_core::plugins::UninstallError) -> ExitCode {
    use norte_core::plugins::UninstallError as U;
    let msg = match e {
        U::InvalidId => norte_i18n::t("cli-plugin-uninstall-invalid-id"),
        U::NotInstalled(id) => norte_i18n::ta("cli-plugin-uninstall-not-installed", &[("id", id)]),
        U::Io(io) => norte_i18n::ta("cli-plugin-uninstall-io", &[("error", &io.to_string())]),
    };
    eprintln!("{msg}");
    ExitCode::FAILURE
}

/// `norte plugin list`: una fila por plugin instalado, tabulada.
async fn plugin_list(backend: &Backend) -> anyhow::Result<ExitCode> {
    let listado = backend.plugins_list().await?;
    if listado.plugins.is_empty() && listado.errors.is_empty() {
        println!("{}", norte_i18n::t("cli-plugin-list-empty"));
        return Ok(ExitCode::SUCCESS);
    }
    for p in &listado.plugins {
        let (nombre, _) = norte_frontend::display_name(p.name.as_bytes());
        let aprobado = norte_i18n::t(if p.approved {
            "cli-plugin-state-approved"
        } else {
            "cli-plugin-state-unapproved"
        });
        let activado = norte_i18n::t(if p.enabled {
            "cli-plugin-state-enabled"
        } else {
            "cli-plugin-state-disabled"
        });
        let caps = if p.capabilities.is_empty() {
            "-".to_string()
        } else {
            p.capabilities.join(",")
        };
        println!(
            "{}\t{}\t{aprobado}\t{activado}\t{caps}\t{nombre}",
            p.id, p.category
        );
    }
    if !listado.errors.is_empty() {
        let n = listado.errors.len().to_string();
        eprintln!(
            "{}",
            norte_i18n::ta("cli-plugin-list-broken", &[("count", &n)])
        );
    }
    Ok(ExitCode::SUCCESS)
}
