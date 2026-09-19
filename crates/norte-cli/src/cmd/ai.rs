//! `norte ai rename` y `norte gc` (M4-A2 / #11, ADR 0031 / ADR 0012).

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use norte_core::backend::Backend;
use norte_proto::TaskState;
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

use crate::cmd::compare::marcado;
use crate::cmd::connect::vpath;
use crate::{AiCmd, AvisoDeJournalPorStderr};

/// `norte ai rename`: sugiere un rename por lote REVISABLE (M4-A2, ADR
/// 0031). Embebido: construye un engine con el proveedor de `[ai]`, pide el
/// plan (el gate opt-in/local-only/denied-paths corta ANTES de que ningún
/// nombre salga), lo IMPRIME y confirma antes de aplicar. Aplicar = N
/// `move_` gobernados (journal + undo + policy) — el plan es el producto.
pub(crate) async fn ai_cmd(cmd: AiCmd) -> anyhow::Result<ExitCode> {
    let AiCmd::Rename {
        dir,
        instruction,
        yes,
    } = cmd;
    let dir = vpath(&dir)?;

    // #167: este subcomando NO pasa por `make_backend` —arma su propio engine—
    // y renombra un directorio entero con los nombres que propuso un MODELO.
    // Es, de todos los caminos embebidos, el que más falta le hace quedar
    // registrado, así que lleva journal como los demás. Perezoso como los demás
    // también (#177): planificar es leer, y leer no le quita el journal a
    // nadie; el lock se toma abajo, a un paso de renombrar.
    let engine = norte_core::embedded::engine_in(&norte_core::connect::config_dir());
    // Este brazo no pasa por `run`, así que instala el suyo — ver
    // `AvisoDeJournalPorStderr`.
    engine.set_journal_warning_sink(Arc::new(AvisoDeJournalPorStderr));
    engine.register_provider(Arc::new(LocalProvider::os_root()) as Arc<dyn Provider>);
    engine.set_connector(Arc::new(norte_core::connect::ConnectionManager::new(
        norte_core::connect::config_dir(),
    )));

    let config = tokio::task::spawn_blocking(norte_core::ai::AiConfig::load)
        .await
        .context("carga de [ai]")?
        .context("[ai] inválido en norte.toml")?;
    let Some(pcfg) = config.rename_provider_config().cloned() else {
        anyhow::bail!(
            "sin proveedor de IA para el rename: define [ai.providers.<n>] y \
             rename_provider en norte.toml (ADR 0031)"
        );
    };
    // El core resuelve el secreto (env → keyring → age) y construye el
    // proveedor; el CLI no toca norte-connect ni ve la clave (regla 10).
    let provider = norte_core::ai::resolve_and_build(&pcfg, norte_core::connect::config_dir())
        .await
        .map_err(|e| anyhow::anyhow!("proveedor de IA: {e}"))?;
    engine.set_ai_provider(provider);
    engine.set_ai_config(config);

    let plan = engine
        .ai_rename_plan(&dir, &instruction)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if plan.entries.is_empty() {
        println!("{}", norte_i18n::t("cli-ai-rename-empty"));
        return Ok(ExitCode::SUCCESS);
    }
    println!("{}", norte_i18n::t("cli-ai-rename-plan"));
    // Nombres controlados por el MODELO: enmascarar hazards de terminal
    // (bidi/invisibles → �) y MARCAR el enmascarado, como TUI/GUI. Un reply
    // UTF-8 válido puede traer RLO y spoofear el prompt de confirmación.
    let masked = |bytes: &[u8]| {
        let (texto, hostil) = norte_frontend::display_name(bytes);
        marcado(&texto, hostil)
    };
    for e in &plan.entries {
        println!(
            "  {} → {}",
            masked(e.from.as_bytes()),
            masked(e.to.as_bytes())
        );
    }

    // El journal se abre AQUÍ, antes de preguntar y antes de renombrar, y no en
    // el primer rename: lo que se está decidiendo es si un modelo renombra un
    // directorio entero, y «esto no se va a poder deshacer» es parte de la
    // pregunta, no una nota a pie después del sí. FUERA del `if !yes`: con
    // `--yes` no hay pregunta que completar, pero sigue habiendo un humano (o
    // un script cuyo log alguien lee) al que le toca enterarse, y ese es
    // justamente el camino donde nadie está mirando la pantalla.
    //
    // El motivo lo acaba de decir el sink de stderr; aquí va la consecuencia —
    // y desde #178 hay DOS consecuencias distintas, que un `bool` confundía.
    //
    // Con `Failed` los renombrados no van a ocurrir: `Engine::gate` los rehúsa
    // uno a uno. Preguntar «¿seguro? no se podrán deshacer» y renombrar cero
    // ficheros saliendo con éxito es lo peor de los dos mundos: un `norte ai
    // rename --yes && <lo siguiente>` en un cron seguiría adelante sobre un
    // no-op silencioso. Así que se para aquí, con el código de los rechazos.
    match engine.journal_obstacle().await {
        Some(norte_core::embedded::NoJournal::Failed(_)) => {
            eprintln!("norte: {}", norte_i18n::t("cli-ai-rename-refused"));
            return Ok(ExitCode::from(2));
        }
        // `Busy` (y cualquier motivo futuro) sí muta, sin quedar registrado:
        // eso es un aviso, no un motivo para no renombrar.
        Some(_) => eprintln!("norte: {}", norte_i18n::t("cli-ai-rename-unjournalled")),
        None => {}
    }

    if !yes {
        use std::io::Write as _;
        eprint!("{} ", norte_i18n::t("cli-ai-rename-confirm"));
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
        let ans = line.trim().to_ascii_lowercase();
        if ans != "y" && ans != "s" {
            println!("{}", norte_i18n::t("cli-ai-rename-abort"));
            return Ok(ExitCode::SUCCESS);
        }
    }

    // Aplica cada entrada como un `move_` del engine, que desde #167 lleva
    // journal (y por tanto undo) salvo que otro proceso tenga el lock. La
    // policy NO: este engine embebido no instala ninguna y gatea con `AllowAll`
    // — quien decide aquí es el humano que acaba de decir que sí al plan.
    let mut ok = 0usize;
    for e in &plan.entries {
        let to = dir.join(e.to.clone());
        let from = dir.join(e.from.clone());
        match engine.move_(&from, &to).await {
            Ok(task) => match task.join().await {
                TaskState::Completed => ok += 1,
                other => eprintln!(
                    "norte: {} → {}: {other:?}",
                    masked(e.from.as_bytes()),
                    masked(e.to.as_bytes())
                ),
            },
            Err(err) => eprintln!(
                "norte: {} → {}: {err}",
                masked(e.from.as_bytes()),
                masked(e.to.as_bytes())
            ),
        }
    }
    println!(
        "{}",
        norte_i18n::ta("cli-ai-rename-done", &[("n", &ok.to_string())])
    );
    // Un rename que no llegó a hacerse NO sale con éxito. Cada fallo ya salió
    // por stderr, pero un script solo mira el código: éxito sobre «cero de
    // cuarenta» es la clase de mentira que encadena un `&&` con lo siguiente.
    Ok(if ok == plan.entries.len() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// `norte gc`: barre staging `.norte-partial` huérfano (#11, ADR 0012).
/// Solo embebido: el wire no expone (aún) el GC — con `--daemon` el error
/// es accionable, no un `Unsupported` seco.
pub(crate) async fn gc_cmd(
    backend: &Backend,
    daemon: bool,
    path: &std::path::Path,
    older_than_hours: u64,
) -> anyhow::Result<ExitCode> {
    let dir = vpath(path)?;
    let older = std::time::Duration::from_secs(older_than_hours.saturating_mul(3600));
    match backend.gc_partials(&dir, older).await {
        Ok(n) => {
            println!(
                "{}",
                norte_i18n::ta(
                    "cli-gc-result",
                    &[("n", &n.to_string()), ("dir", &dir.display_lossy())],
                )
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(norte_proto::Error::Unsupported) if daemon => {
            anyhow::bail!("{}", norte_i18n::t("cli-gc-remote-unsupported"))
        }
        Err(e) => Err(anyhow::anyhow!("{e}")),
    }
}
