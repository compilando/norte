//! `norte plugin run|install|uninstall|list` (M4-P4).

use std::process::ExitCode;

use norte_core::backend::Backend;

use crate::PluginCmd;

/// `norte plugin run <id> <command> [arg]`: ejecuta un comando de un plugin YA
/// aprobado+activado por el humano y escribe su salida a STDOUT. Va por el
/// `Backend` elegido con los flags globales (`--daemon`/`--socket`), como el
/// resto de operaciones (regla 7).
pub(crate) async fn plugin_cmd(backend: &Backend, cmd: PluginCmd) -> anyhow::Result<ExitCode> {
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
            match norte_core::plugins::install(&dir, &path, force) {
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
        // Simétrico de `Install`: sin daemon. El id llega de la línea de
        // comandos y `uninstall` lo valida antes de convertirlo en ruta.
        PluginCmd::Uninstall { id } => {
            let dir = norte_core::connect::config_dir();
            match norte_core::plugins::uninstall(&dir, &id) {
                Ok(rep) => {
                    println!(
                        "{}",
                        norte_i18n::ta("cli-plugin-uninstalled", &[("id", &rep.id)])
                    );
                    if rep.was_approved {
                        println!("{}", norte_i18n::t("cli-plugin-uninstalled-consent"));
                    }
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    // Texto al usuario por Fluent (#319); el `Display` del
                    // error se queda para los logs.
                    use norte_core::plugins::UninstallError as U;
                    let msg = match &e {
                        U::InvalidId => norte_i18n::t("cli-plugin-uninstall-invalid-id"),
                        U::NotInstalled(id) => {
                            norte_i18n::ta("cli-plugin-uninstall-not-installed", &[("id", id)])
                        }
                        U::Io(io) => {
                            norte_i18n::ta("cli-plugin-uninstall-io", &[("error", &io.to_string())])
                        }
                    };
                    eprintln!("{msg}");
                    Ok(ExitCode::FAILURE)
                }
            }
        }
        // `plugin.list`, el mismo catálogo que pinta el gestor: id, categoría,
        // los DOS hechos (aprobado, activado) y las capabilities que aprobar
        // concedería. El nombre viene de un tercero: saneado, como en el
        // gestor. Los rotos se cuentan, no se listan — `norte doctor` los
        // explica uno a uno.
        PluginCmd::List => plugin_list(backend).await,
    }
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
