//! `norte ai rename` y `norte gc` (M4-A2 / #11, ADR 0031 / ADR 0012).

use std::process::ExitCode;
use std::sync::Arc;

use norte_core::backend::Backend;

use crate::cmd::compare::marcado;
use crate::cmd::connect::vpath;
use crate::{AiCmd, AvisoDeJournalPorStderr};

/// `norte ai rename`: sugiere un rename por lote REVISABLE (M4-A2, ADR
/// 0031). Embebido: construye un engine con el proveedor de `[ai]`, pide el
/// plan (el gate opt-in/local-only/denied-paths corta ANTES de que ningún
/// nombre salga), lo IMPRIME y confirma antes de aplicar. Aplicar = UN lote
/// `fs.rename_batch` (una Task, una unidad de undo), como en la TUI y la
/// ventana — el plan es el producto.
pub(crate) async fn ai_cmd(cmd: AiCmd) -> anyhow::Result<ExitCode> {
    let AiCmd::Rename {
        dir,
        instruction,
        yes,
    } = cmd;
    let dir = vpath(&dir)?;
    // Desde aquí todo va por `Backend`, el mismo camino que la TUI y la
    // ventana: la IA propone, `fs.rename_batch_plan` decide si se puede y
    // `fs.rename_batch` aplica (regla 7).
    let backend = backend_con_ia().await?;

    let plan = backend
        .ai_rename_plan(&dir, &instruction, &[])
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
    // Un nombre por línea, con las mismas claves que el modal de la TUI:
    // `a → b` en una sola dejaba que un fichero llamado `x → y` —imprimible
    // corriente, `display_name` no lo enmascara— fingiera la pareja entera,
    // justo en la pantalla que se lee antes de contestar «sí».
    for (i, e) in plan.entries.iter().enumerate() {
        println!(
            "  {}",
            norte_i18n::ta(
                "modal-ai-rename-pair-from",
                &[
                    ("n", &(i + 1).to_string()),
                    ("from", &masked(e.from.as_bytes()))
                ],
            )
        );
        println!(
            "     {}",
            norte_i18n::ta(
                "modal-ai-rename-pair-to",
                &[("to", &masked(e.to.as_bytes()))]
            )
        );
    }

    // El plan de la IA es INTENCIÓN; si se puede ejecutar lo decide el
    // planificador de lotes del core, que es el que sabe romper un ciclo
    // (`a↔b`) con un temporal y el que ve las colisiones con lo que ya hay.
    // Aplicar entrada a entrada fallaba en cada intercambio y dejaba a medias
    // cualquier otro plan con un choque.
    //
    // Códigos: 2 es «rehusado, no se tocó nada» (plan inválido, colisiones,
    // journal ilegible, plan caducado), como en `norte sync`; 1 es «el lote
    // corrió y falló o se deshizo».
    let Some(pairs) = norte_frontend::rename_pairs(&plan.entries) else {
        eprintln!("norte: {}", norte_i18n::t("cli-ai-rename-invalid"));
        return Ok(ExitCode::from(2));
    };
    let lote = backend
        .rename_batch_plan(&dir, &pairs)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if !lote.executable {
        eprintln!("norte: {}", norte_i18n::t("cli-ai-rename-collisions"));
        let revisado = norte_frontend::BatchPlan::Ready(Box::new(lote));
        for parte in revisado.detail_parts(pairs.len(), norte_i18n::active()) {
            for linea in detalle(parte) {
                eprintln!("  {linea}");
            }
        }
        return Ok(ExitCode::from(2));
    }
    let reales = lote.steps.iter().filter(|s| !s.temp).count();

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
    match backend.journal_obstacle().await {
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

    // UN lote, UNA Task y UNA unidad deshacible del journal (ADR 0042), con el
    // `plan_hash` de lo que se acaba de enseñar: si el directorio cambió
    // mientras el humano leía, el core contesta `PlanStale` y no toca nada.
    // La policy no entra: este engine embebido gatea con `AllowAll` — quien
    // decide aquí es el humano que acaba de decir que sí al plan.
    let task = match backend.rename_batch(&dir, &pairs, &lote.plan_hash).await {
        Ok(task) => task,
        Err(norte_proto::Error::PlanStale) => {
            eprintln!("norte: {}", norte_i18n::t("cli-ai-rename-stale"));
            return Ok(ExitCode::from(2));
        }
        Err(e) => return Err(anyhow::anyhow!("{e}")),
    };
    let id = task.id();
    let salida = crate::task::run_task(task, false).await;
    informar_del_lote(&backend, id, salida, reales).await
}

/// `run_task` dice cómo terminó la Task; lo que de verdad pasó en el disco
/// lo dice el INFORME, y se pide siempre: un lote `Completed` con un paso
/// atascado es justo lo que el desenlace de la Task no cuenta. Las líneas son
/// las mismas que el diálogo de la ventana (`batch_report_lines`), y cada ruta
/// va sola en la suya.
async fn informar_del_lote(
    backend: &Backend,
    id: norte_proto::TaskId,
    salida: ExitCode,
    reales: usize,
) -> anyhow::Result<ExitCode> {
    match backend.rename_batch_report(id).await {
        Ok(informe) if norte_frontend::batch_report_is_clean(&informe) => {
            if salida == ExitCode::SUCCESS {
                println!(
                    "{}",
                    norte_i18n::ta("cli-ai-rename-done", &[("n", &reales.to_string())])
                );
            }
        }
        Ok(informe) => {
            for linea in norte_frontend::batch_report_lines(&informe, norte_i18n::active()) {
                match linea {
                    norte_frontend::BatchReportLine::Phrase(texto) => eprintln!("norte: {texto}"),
                    norte_frontend::BatchReportLine::Path(p) => {
                        let (texto, hostil) = norte_frontend::path_display(&p);
                        eprintln!("    {}", marcado(&texto, hostil));
                    }
                }
            }
            return Ok(ExitCode::FAILURE);
        }
        Err(_) if salida != ExitCode::SUCCESS => {
            eprintln!("norte: {}", norte_i18n::t("modal-batch-report-failed"));
        }
        Err(_) => {}
    }
    Ok(salida)
}

/// El engine embebido de `norte ai rename`, con el proveedor de `[ai]`.
///
/// #167: este subcomando NO pasa por `make_backend` —arma su propio engine—
/// y renombra un directorio entero con los nombres que propuso un MODELO. Es,
/// de todos los caminos embebidos, el que más falta le hace quedar
/// registrado, así que lleva journal como los demás. Perezoso como los demás
/// también (#177): planificar es leer, y leer no le quita el journal a nadie;
/// el lock se toma a un paso de renombrar.
async fn backend_con_ia() -> anyhow::Result<Backend> {
    let dir = norte_core::connect::config_dir();
    let engine = norte_core::embedded::engine_in(&dir);
    // Este brazo no pasa por `run`, así que instala el suyo — ver
    // `AvisoDeJournalPorStderr`.
    engine.set_journal_warning_sink(Arc::new(AvisoDeJournalPorStderr));
    // Lo que lleva todo engine, IA incluida (`norte_core::equipo`). El core
    // resuelve el secreto (env → keyring → age) y construye el proveedor; la
    // CLI no toca norte-connect ni ve la clave (regla 10).
    let hecho = norte_core::equipo::equipar(&engine, &dir, true).await;
    if let Err(e) = norte_core::archive_config::aplicar(&engine).await {
        eprintln!(
            "{}",
            crate::cmd::daemon::texto_del_aviso(&norte_core::equipo::Aviso::ArchivoInvalido(
                e.to_string()
            ))
        );
    }
    // Aquí la IA no es opcional: es el comando. Lo que en los demás es un
    // aviso, aquí es el motivo de no poder hacer nada.
    if !hecho.ia_renombrado {
        for aviso in &hecho.avisos {
            eprintln!("{}", crate::cmd::daemon::texto_del_aviso(aviso));
        }
        anyhow::bail!(
            "sin proveedor de IA para el rename: define [ai.providers.<n>] y \
             rename_provider en norte.toml (ADR 0031)"
        );
    }
    Ok(Backend::Embedded(Arc::new(engine)))
}

/// Las líneas de una parte del detalle de un plan no aplicable, con el mismo
/// saneado que el modal (`norte_frontend::BatchPlan::detail_parts`): el nombre
/// ofensor es de un tercero y ya viene enmascarado; aquí solo se le pone la
/// marca.
///
/// El nombre va en SU PROPIA línea, como en la TUI (#273): `display_name` no
/// enmascara `✗`, dígitos ni `:`, así que un fichero llamado
/// `✗ 4. ya existe: otro.txt` pegado a su etiqueta fingiría otra entrada.
/// El nombre sale recortado al ancho del modal (`middle_ellipsis`): basta
/// para reconocerlo, y el plan entero ya se imprimió arriba sin recortar.
fn detalle(parte: norte_frontend::DetailPart) -> Vec<String> {
    use norte_i18n::{t, ta};
    match parte {
        norte_frontend::DetailPart::Temp { count } => {
            vec![ta("modal-rename-batch-temp", &[("n", &count.to_string())])]
        }
        norte_frontend::DetailPart::Collision {
            index,
            kind_key,
            name,
            hostile,
        } => {
            let kind = t(kind_key);
            let prefijo = match index {
                Some(n) => ta(
                    "modal-rename-batch-collision-prefix",
                    &[("n", &n.to_string()), ("kind", &kind)],
                ),
                None => ta(
                    "modal-rename-batch-collision-prefix-unindexed",
                    &[("kind", &kind)],
                ),
            };
            vec![prefijo, format!("  {}", marcado(&name, hostile))]
        }
        norte_frontend::DetailPart::More {
            shown,
            total,
            hostile,
        } => vec![marcado(
            &ta(
                "modal-rename-batch-collision-more",
                &[("shown", &shown.to_string()), ("total", &total.to_string())],
            ),
            hostile,
        )],
    }
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
