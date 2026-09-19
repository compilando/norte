//! `norte sync`: planifica, enseña el plan, pregunta y aplica (ADR 0049).

use std::process::ExitCode;

use anyhow::Context;
use norte_core::backend::Backend;
use norte_proto::TaskState;

use crate::SyncCliOpts;
use crate::cmd::compare::{codigo_por_escritura, parse_compare_criteria, rel_marcado};
use crate::cmd::connect::vpath;
use crate::task::{SigintGate, drive_task};

/// `norte sync`: planifica, enseña, pregunta, aplica — TODO en una conexión.
///
/// # Por qué una sola invocación
/// Un plan aprobado se retiene POR CONEXIÓN, en un registro en memoria que
/// nace vacío, y `sync.apply` no lleva nada más que el `plan_hash`. Un CLI que
/// planease en un proceso y aplicara en otro no podría funcionar ni queriendo:
/// el registro del segundo no conoce ese hash. Así que la pregunta se hace con
/// la conexión viva, y `--dry-run` es esta misma función sin la segunda mitad.
///
/// Planifica, drena el stream por [`norte_frontend::sync::SyncState`] —el
/// ÚNICO sitio donde los pasos se cuadran contra los contadores del cierre—,
/// enseña el plan entero, y solo ENTONCES resuelve el journal, pregunta (salvo
/// `--yes`) y aplica. `--dry-run` es esta misma función cortada justo antes de
/// esa resolución: ningún camino que pase por `--dry-run` llega a
/// `Backend::sync_apply`.
///
/// # El spool se desmonta al salir, pase lo que pase
/// Ésta es sólo la envolvente que lo garantiza. `sync.plan` deja en el
/// directorio de estado un fichero con el listado relativo de los DOS árboles
/// (ADR 0049), y el daemon lo recoge en dos sitios que este proceso no tiene:
/// un barrido al arrancar y un `drop_connection` al cerrar cada conexión. Sin
/// esto, un `--dry-run` —que por definición no aplica nada— dejaría el fichero
/// ahí para siempre, y lo mismo cada pregunta contestada que no.
///
/// Va en una función aparte y no al final del cuerpo porque el cuerpo tiene
/// `?`: media docena de caminos de salida, y la limpieza tiene que estar en
/// todos.
pub(crate) async fn sync_cmd(
    backend: &Backend,
    source: &std::path::Path,
    dest: &std::path::Path,
    opts: SyncCliOpts<'_>,
) -> anyhow::Result<ExitCode> {
    // UNA vez por mandato, y antes de cualquier fase: ver `SigintGate`. Armarla
    // por fase deja el prompt `[y/N]` con un `Ctrl+C` que tokio se traga y que
    // ya no mata el proceso (revisión de rama de W2, BLOCKER-1).
    let sigint = SigintGate::arm();
    let salida = sync_plan_show_apply(backend, source, dest, opts, &sigint).await;
    // Un Ctrl+C durante la planificación YA no mata el proceso a las bravas
    // (#180: `sync_plan_show_apply` arma su propio `watch_ctrl_c` y cancela
    // por el token, así que `run_sync_plan` cierra el spool antes de volver
    // aquí). Esta llamada sigue siendo necesaria por lo demás: un `--dry-run`
    // o una pregunta contestada que no también dejarían el plan retenido si
    // nadie lo soltara.
    backend.drop_retained_plans().await;
    salida
}

/// El cuerpo de [`sync_cmd`], con sus salidas tempranas. Ver allí por qué está
/// partido en dos.
async fn sync_plan_show_apply(
    backend: &Backend,
    source: &std::path::Path,
    dest: &std::path::Path,
    opts: SyncCliOpts<'_>,
    sigint: &SigintGate,
) -> anyhow::Result<ExitCode> {
    let source = vpath(source)?;
    let dest = vpath(dest)?;

    // `SyncCompareOptions` sí deriva un `Default` de verdad (a diferencia de
    // `FsCompareParams` en `compare_cmd`), así que no hay un 2000 mágico que
    // repetir aquí.
    let mut compare = norte_proto::methods::SyncCompareOptions {
        criteria: parse_compare_criteria(opts.criteria)?,
        ..norte_proto::methods::SyncCompareOptions::default()
    };
    if let Some(ms) = opts.mtime_tolerance_ms {
        compare.mtime_tolerance_ms = ms;
    }

    let params = norte_proto::methods::SyncPlanParams {
        source,
        dest,
        mode: opts.mode.into(),
        compare,
        // "ausente = del llamante no es" — se deja en su default (Copy), como
        // pide la tarea.
        on_unknown: norte_proto::methods::OnUnknown::default(),
        include: None,
    };

    // La Task nace DENTRO de `sync_plan`, y el `.part` del spool con ella: el
    // `Ctrl+C` que llegue entre una cosa y otra tiene que esperar al handle,
    // no matar el proceso (que se saltaría el `Drop` que borra el `.part`).
    sigint.naciendo();
    let (task, mut rx) = backend
        .sync_plan(params)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::t("cli-sync-failed"))?;

    // Ctrl+C durante el drenaje cancela la Task por su `CancellationToken`
    // (regla dura 3), igual que la mitad de apply — y no por el SIGINT por
    // defecto del SO. Antes de esto, este `while let` no tenía manejador
    // alguno: Ctrl+C mataba el proceso ENTERO antes de que `run_sync_plan`
    // pudiera ver el token y cerrar el spool, así que el `.part` que
    // `sync.plan` deja en `<estado>/sync-spools/` quedaba huérfano para
    // siempre (#180) — el TTL solo barre planes CERRADOS y esta CLI no tiene
    // daemon que lo recoja al arrancar. Cancelado LIMPIO, en cambio,
    // `run_sync_plan` ve el token, corta el flujo y llama
    // `writer.finish(PlanOutcome::Interrupted)`, que sí borra el `.part`.
    sigint.apunta_a(&task);

    // El ÚNICO sitio donde los pasos se cuadran contra `SyncPlanDone::counts`
    // es `SyncState`; montar un `SyncPlan` a mano sería una segunda ocasión de
    // olvidar esa comprobación (la razón de ser de esta tarea).
    let mut state = norte_frontend::sync::SyncState::default();
    while let Some(event) = rx.recv().await {
        match event {
            norte_core::sync::SyncPlanEvent::Steps(batch) => {
                state.on_steps(batch);
            }
            norte_core::sync::SyncPlanEvent::Done(done) => {
                state.on_plan_done(done);
            }
        }
    }
    // La Task terminó (el canal se cerró): el manejador ya no tiene nada que
    // cancelar. Sin este `abort()` el `ctrl_c()` de dentro se queda vivo para
    // siempre, esperando una señal que ya no le sirve a nadie.
    sigint.suelta();

    // El canal se cierra cuando la Task termina, así que este `join` no
    // espera de más. Se exige AMBAS cosas: que el estado haya cerrado
    // (`sync.plan_done` llegó) Y que la Task terminara `Completed`. Un canal
    // que se cierra con el diálogo aún en `Planning` — la Task murió,
    // canceló, o el buffer de este proceso se llenó y el enrutado cerró el
    // feed (ver la rustdoc de `Backend::sync_plan`) — es exactamente el "no
    // se pudo saber" que no puede confundirse con "sin diferencias": sin
    // `sync.plan_done` no hay `plan_hash` y no hay nada que aprobar.
    let task_state = task.join().await;
    let plan = match state {
        norte_frontend::sync::SyncState::Ready(plan) if task_state == TaskState::Completed => plan,
        _ => {
            eprintln!(
                "norte: {}",
                norte_i18n::ta(
                    "cli-sync-incomplete",
                    &[("state", &format!("{task_state:?}"))],
                )
            );
            return Ok(ExitCode::from(2));
        }
    };

    // Un plan BLOQUEADO va PRIMERO, antes que la comprobación de vacío: el
    // wire garantiza que `!executable` ⟹ `steps` vacío, así que leerlo por la
    // lista de pasos diría «nada que sincronizar» y contestaría 0 sobre un
    // plan que se paró por una colisión de nombres o un destino de solo
    // lectura. Es el mismo fallo que el tercer código existe para no cometer,
    // y del lado que escribe.
    if !plan.done().executable {
        return Ok(report_blockers(plan.done()));
    }

    if plan.steps().is_empty() {
        // Ejecutable, íntegro y sin un solo paso: los dos árboles ya coinciden.
        println!("{}", norte_i18n::t("cli-sync-empty"));
        return Ok(ExitCode::SUCCESS);
    }

    if let Err(e) = print_plan(&plan) {
        return Ok(codigo_por_escritura(&e));
    }

    // Los pasos que se acaban de enseñar no cuadran con lo que el plan dice
    // ser. Se enseñan igual —son la explicación— pero no se aplica: el plan
    // que `sync.apply` ejecutaría es el RETENIDO, entero, y aprobar una lista
    // que no es esa es aprobar a ciegas. Vale también para `--dry-run`: un
    // plan que no se puede enseñar entero tampoco se ha «enseñado».
    if !plan.integrity().is_complete() {
        eprintln!(
            "norte: {}",
            norte_i18n::ta(
                "cli-sync-integrity",
                &[("detail", &format!("{:?}", plan.integrity()))],
            )
        );
        return Ok(ExitCode::from(2));
    }

    if opts.dry_run {
        return Ok(ExitCode::from(1));
    }
    sync_apply_and_report(backend, &plan, opts.yes, sigint).await
}

/// Enseña por qué un plan no se puede ejecutar, y devuelve el código con el
/// que se sale de ahí (siempre 2: no ocurrió nada).
///
/// A stderr porque no es el plan —el plan no existe: `!executable` ⟹ `steps`
/// vacío— sino la explicación de que no lo haya.
///
/// Cada bloqueo son TRES líneas —ruta, ancla si consta, y motivo— nunca una
/// sola con `: ` en medio: era la misma forma que se le quitó a
/// `cli-sync-failure`, y un nombre puede fingirla (corpus `cause_join_spoof`,
/// #189). El ancla importa porque tres de las cuatro clases nombradas
/// —`AmbiguousDest`, `DestReadOnly`, `DirTooLarge`— nombran el DESTINO por
/// definición, y antes de `blocker_anchor` esta lista las leía con la
/// reinterpretación del origen (#152 reproducido contra tres rutas del otro
/// árbol).
fn report_blockers(done: &norte_proto::methods::SyncPlanDone) -> ExitCode {
    eprintln!("norte: {}", norte_i18n::t("cli-sync-blocked"));
    let lang = norte_i18n::active();
    let enc = norte_frontend::sync::SyncEncodings::default();
    for blocker in &done.blockers {
        let anchor = norte_frontend::sync::blocker_anchor(blocker);
        // Y no `rel_display` a secas: un bloqueo que no es de un sitio
        // concreto —un destino de solo lectura— trae la RAÍZ (`rel` vacío),
        // y `rel_display` sola pinta eso como nada. `rel_display_or_root` es
        // el contrato que `RelDisplay::text` documenta y que ningún painter
        // cumplía (#193): «todo el árbol», no una línea en blanco.
        let rel =
            norte_frontend::sync::rel_display_or_root(&blocker.rel, enc.for_anchor(anchor), lang);
        eprintln!(
            "  {}",
            norte_i18n::ta("cli-sync-blocker", &[("rel", &rel_marcado(&rel))])
        );
        if let Some(q) = norte_frontend::sync::anchor_label(anchor, lang) {
            eprintln!("    {q}");
        }
        eprintln!(
            "    {}",
            norte_i18n::ta(
                "cli-sync-blocker-why",
                &[(
                    "why",
                    &norte_frontend::sync::blocker_label(blocker.kind, lang),
                )],
            )
        );
    }
    // La lista viene CAPADA (`SYNC_MAX_BLOCKERS_REPORTED`) y el total no:
    // callar la diferencia haría creer que se han visto todos.
    let mostrados = u64::try_from(done.blockers.len()).unwrap_or(u64::MAX);
    if done.blockers_total > mostrados {
        eprintln!(
            "  {}",
            norte_i18n::ta(
                "cli-sync-blockers-more",
                &[(
                    "n",
                    &done.blockers_total.saturating_sub(mostrados).to_string(),
                )],
            )
        );
    }
    ExitCode::from(2)
}

/// Escribe el plan ENTERO —cabecera, un paso por línea, y el resumen— a
/// stdout.
///
/// Nombres que este proceso no controla del todo (el destino puede deletrear
/// una entrada distinto del origen, #152): MARCAR el enmascarado, igual que
/// `ai_cmd` y `compare_cmd` — ver [`rel_marcado`].
///
/// Por un `BufWriter` que se suelta al volver: bufferizado para no pagar una
/// syscall por paso, y devolviendo el error de escritura en vez de hacer
/// `panic!` como haría `println!` (`| head` sobre un plan de diez mil pasos es
/// la forma normal de asomarse a él). Que el lock se suelte AQUÍ importa: lo
/// que se imprima después —la pregunta, el informe— no puede adelantarse al
/// plan.
///
/// # Errors
/// Lo que diga la escritura a stdout; el llamante lo traduce con
/// [`codigo_por_escritura`].
/// Las LÍNEAS de un paso del plan: una por campo, nunca una unida.
///
/// Pura y separada de [`print_plan`] para poder pinearla — el e2e solo alcanza
/// pasos sin `dest_rel`, que es justo la rama que no falla.
///
/// **Un campo por línea** (auditoría de encoding de la revisión de rama de C2,
/// MAJOR-2). ` → ` y `  (…)` son imprimibles corrientes que
/// `display_name_with` no enmascara, así que llegan SIN el `!` de
/// [`rel_marcado`]: un fichero llamado `a → mem_b.txt` —corpus
/// `arrow_join_spoof`— fingía la pareja entera, y uno llamado
/// `backup  (unreadable)` fingía el VEREDICTO, en la lista que el humano
/// repasa buscando qué se borra. Y aquí pesa más que en el informe: el informe
/// es posterior, esto es la pantalla ANTES del `y`. El salto de línea sí es un
/// separador que un nombre no puede falsificar — `\n` es Cc y
/// `is_terminal_hazard` lo enmascara a `U+FFFD`.
///
/// LOS TRES glifos van en la primera, los mismos que la TUI: el del medio es
/// la CONFIANZA de la comparación que produjo el paso —o sea «esta
/// sobrescritura se decide sólo por la fecha»— y ésta es la pantalla en la que
/// un humano dice que sí a borrar un subárbol.
fn plan_step_lines(cells: &norte_frontend::sync::StepCells) -> Vec<String> {
    let lang = norte_i18n::active();
    let mut lineas = vec![format!(
        "{}{}{} {}",
        cells.glyphs.kind,
        cells.glyphs.confidence,
        cells.glyphs.undo,
        rel_marcado(&cells.rel)
    )];
    // El ancla, cuando la ruta NO cuelga del origen. En una lista donde una
    // ruta sin calificar significa «del origen», callarlo lo AFIRMA — y el
    // `rel` de un `DeleteTree` cuelga del destino (MAJOR-1: la CLI era el
    // único de los tres painters que tiraba este campo).
    if let Some(q) = norte_frontend::sync::anchor_label(cells.anchor, lang) {
        lineas.push(format!("  {q}"));
    }
    if let Some(d) = &cells.dest_rel {
        lineas.push(format!(
            "  {}",
            norte_i18n::ta("cli-sync-step-dest", &[("dest", &rel_marcado(d))])
        ));
        // Las dos mitades pintan igual (un par NFC/NFD, típicamente) sin que
        // ninguna llegue hostil: sin esto la CLI repite la misma cadena en
        // dos líneas y nada explica por qué (#192).
        if let Some(q) = norte_frontend::sync::dest_twin_label(cells.dest_rel_twin, lang) {
            lineas.push(format!("  {q}"));
        }
    }
    // El porqué de una omisión, o de un undo que no devolvería el fichero.
    if let Some(r) = cells.reason {
        lineas.push(format!(
            "  {}",
            norte_i18n::ta(
                "cli-sync-step-reason",
                &[("reason", &norte_frontend::sync::reason_label(r, lang))]
            )
        ));
    }
    lineas
}

fn print_plan(plan: &norte_frontend::sync::SyncPlan) -> std::io::Result<()> {
    use std::io::Write as _;

    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    writeln!(out, "{}", norte_i18n::t("cli-sync-plan"))?;
    for step in plan.steps() {
        // Sin reinterpretación por lado: el CLI no tiene panes, así que los
        // nombres se leen como vienen (`SyncEncodings::default()`).
        let cells = norte_frontend::sync::render_step(
            step,
            plan.dest_trash(),
            norte_frontend::sync::SyncEncodings::default(),
        );
        for linea in plan_step_lines(&cells) {
            writeln!(out, "{linea}")?;
        }
    }
    for line in plan.summary_lines(norte_i18n::active()) {
        writeln!(out, "{line}")?;
    }
    out.flush()
}

/// La segunda mitad de `norte sync` (tarea 3): resolver el journal, preguntar
/// salvo `--yes`, aplicar y contar. Separada de [`sync_cmd`] por longitud, no
/// por independencia — solo se llama desde ahí, con el plan que ACABA de
/// imprimirse, así que no hay camino que la alcance sin que el plan entero ya
/// estuviera en pantalla.
async fn sync_apply_and_report(
    backend: &Backend,
    plan: &norte_frontend::sync::SyncPlan,
    yes: bool,
    sigint: &SigintGate,
) -> anyhow::Result<ExitCode> {
    // Ni un paso que escriba: todo lo que el plan trae son omisiones. No hay
    // nada que aprobar (`SyncPlan::can_approve` lo dice también así) y aplicar
    // no cambiaría un byte, pero tampoco se ha resuelto la diferencia que las
    // provocó — así que no es un 0.
    if plan.acting() == 0 {
        eprintln!("norte: {}", norte_i18n::t("cli-sync-nothing-to-apply"));
        return Ok(ExitCode::from(2));
    }

    // El journal se resuelve AQUÍ, antes de preguntar y antes de escribir, y no
    // en la primera mutación: lo que se está decidiendo es si se reescribe un
    // subárbol, y «esto no se va a poder deshacer» es parte de la pregunta, no
    // una nota a pie después del sí. FUERA del `if !yes` porque con `--yes` no
    // hay pregunta que completar pero sigue habiendo un log que alguien lee, y
    // ese es justamente el camino donde nadie mira la pantalla.
    //
    // Y se PARA, no se avisa: `Engine::sync_apply_as` se niega igual unas
    // líneas más abajo, así que seguir sólo cambia dónde aparece el «no» y
    // quién lo entiende.
    //
    // **Dos motivos, dos frases** (#178). El caso corriente es que el journal
    // lo tenga OTRO: el embebido es el MISMO `journal.db` que el daemon abre en
    // exclusiva, así que cualquiera con un `ntc` o un daemon vivo cae ahí, y el
    // remedio —hablar con ese daemon en vez de pelearle el fichero— es
    // `--daemon`. Pero con un journal ILEGIBLE ese remedio no existe: `norte
    // daemon run` se niega a arrancar con ese mismo fichero, así que mandar al
    // usuario a `--daemon` sería mandarlo a otra pared. Decirle cuál de las dos
    // paredes tiene delante es toda la diferencia entre un mensaje accionable y
    // uno que hace perder media hora.
    match backend.journal_obstacle().await {
        Some(norte_core::embedded::NoJournal::Failed(_)) => {
            eprintln!("norte: {}", norte_i18n::t("cli-sync-journal-unreadable"));
            return Ok(ExitCode::from(2));
        }
        Some(_) => {
            eprintln!("norte: {}", norte_i18n::t("cli-sync-unjournalled"));
            return Ok(ExitCode::from(2));
        }
        None => {}
    }

    if !yes {
        use std::io::{IsTerminal as _, Write as _};
        // Sin terminal no hay a quién preguntar, y una pregunta que nadie va a
        // contestar no se hace: se rehúsa ANTES, como el prompt TOFU de este
        // mismo fichero. Leer el EOF de un `< /dev/null` como una negativa
        // sería igual de correcto en cuanto a lo que se escribe (nada) y mucho
        // peor de explicar, porque el humano que montó el cron no está aquí
        // para leerlo; que salga nombrando `--yes` sí lo lee mañana en el log.
        if !std::io::stdin().is_terminal() {
            eprintln!("norte: {}", norte_i18n::t("cli-sync-noninteractive"));
            return Ok(ExitCode::from(2));
        }
        // La SEGUNDA pregunta, cuando el plan la merece (borra árboles del
        // destino o el undo no lo devuelve todo): `SyncPlan::confirmation` ya
        // la redacta a partir de `dest_trash` y los contadores — no hay una
        // segunda frase sobre borrado que escribir aquí sin arriesgarse a que
        // diga algo distinto de lo que el resumen ya dijo. Que aparezca
        // depende de `can_approve`, y sus tres condiciones están comprobadas
        // antes de llegar aquí: si no lo estuvieran, el plan MENOS fiable sería
        // justo el que preguntara con un `[s/N]` pelado.
        if let Some(confirmation) = plan.confirmation(norte_i18n::active()) {
            eprintln!("{}", confirmation.text);
        }
        eprint!("{} ", norte_i18n::t("cli-sync-confirm"));
        std::io::stderr().flush().ok();
        // stdin es bloqueante: fuera del reactor (regla 2).
        let line = tokio::task::spawn_blocking(|| {
            let mut s = String::new();
            std::io::stdin().read_line(&mut s).map(|_| s)
        })
        .await
        .context(norte_i18n::t("cli-confirm-read"))??;
        let ans = line.trim().to_ascii_lowercase();
        if ans != "y" && ans != "s" {
            // Un «no» NO es «los árboles están sincronizados». Sale por el
            // mismo código que todo lo demás que no llegó a escribir, que es
            // lo que un `norte sync src dst && echo ok` necesita para no
            // mentir.
            println!("{}", norte_i18n::t("cli-sync-abort"));
            return Ok(ExitCode::from(2));
        }
    }

    // Misma ventana que en la planificación: la Task de apply ya está
    // ESCRIBIENDO antes de que `drive_task` la apunte, y un `Ctrl+C` ahí
    // mataba el proceso en crudo — sin cancelación limpia y sin el
    // `.norte-partial` que la regla dura 3 promete.
    sigint.naciendo();
    let apply_task = backend
        .sync_apply(&plan.done().plan_hash)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::t("cli-sync-failed"))?;
    let task_id = apply_task.id();
    // `drive_task` y no `run_task`: Ctrl+C tiene que cancelar LIMPIO por el
    // `CancellationToken` de la Task (regla dura 3), no matar el proceso a
    // medio escribir un árbol — la trampa que la propia CLAUDE.md nombra
    // («cancelar una copia debe dejar un destino limpio o un
    // `.norte-partial`, nunca un parcial sin marcar»). Hasta ahí es lo mismo
    // que `cp`/`mv`/`rm`/`undo`.
    //
    // Donde diverge (#187): `run_task` traduciría un `Cancelled` a «destino
    // limpio» y volvería sin pedir el informe — cierto para esos cuatro
    // comandos, falso aquí. Un `sync.apply` cancelado deja lo aplicado hasta
    // el corte JOURNALIZADO (regla dura 4), y `sync.report` es la única forma
    // de decir cuánto: la TUI y la GUI ya lo piden siempre que la Task
    // termina, cancelación incluida (`harvest_sync_apply`,
    // `norte_frontend::sync::SyncView::on_apply_ended`). Este comando era el
    // único de los tres frontends que no podía decirlo.
    let final_state = drive_task(apply_task, true, Some(sigint)).await;
    match &final_state {
        TaskState::Completed => {}
        TaskState::Cancelled => {
            eprintln!("{}", norte_i18n::t("cli-sync-cancelled"));
        }
        TaskState::Failed { error } => {
            eprintln!(
                "{}",
                norte_i18n::ta("cli-final-error", &[("error", &error.to_string())])
            );
        }
        other => {
            // El canal de progreso se cerró sin que la Task llegara a un
            // desenlace terminal (la conexión murió a medio camino): no hay
            // nada fiable que pedir, mismo criterio que la rama equivalente de
            // `run_task`.
            eprintln!(
                "{}",
                norte_i18n::ta("cli-unexpected-state", &[("state", &format!("{other:?}"))])
            );
            return Ok(ExitCode::from(2));
        }
    }
    let report = backend
        .sync_report(task_id)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::t("cli-sync-failed"))?;

    print_sync_report(&report);

    // El ÚNICO 1 de este comando: un apply que TERMINÓ (`Completed`) y no
    // dejó ningún fallo detrás. Todo lo demás —lo que no se pudo planificar,
    // lo que no se aprobó, lo que no se pudo aplicar, lo que se canceló y lo
    // que se aplicó a medias— es 2. Antes de #187 una `Task` que `Failed`
    // salía por el 1 de `ExitCode::FAILURE` de `run_task` — el MISMO código
    // que un apply limpio sin fallos, colisión que el `match` de arriba ya
    // resolvió devolviendo 2 antes de llegar hasta aquí para todo lo que no
    // sea `Completed`.
    Ok(ExitCode::from(
        if matches!(final_state, TaskState::Completed) && report.failed == 0 {
            1
        } else {
            2
        },
    ))
}

/// La línea de cuentas y la lista de fallos de un
/// [`SyncReportResult`](norte_proto::methods::SyncReportResult), en el
/// mismo formato pase lo que pase (#187): un informe cancelado a medias se
/// imprime IGUAL que uno completo, porque lo aplicado hasta el corte es tan
/// real como lo demás.
///
/// Separada de [`sync_apply_and_report`] solo por longitud (`too_many_lines`
/// de clippy) — no hay un segundo llamante.
fn print_sync_report(report: &norte_proto::methods::SyncReportResult) {
    println!(
        "{}",
        norte_i18n::ta(
            "cli-sync-done",
            &[
                ("done", &report.done.to_string()),
                ("failed", &report.failed.to_string()),
                ("skipped", &report.skipped.to_string()),
            ],
        )
    );
    // Y si esto se puede devolver o no (#208). La CLI es el lector que NUNCA
    // tuvo el `sync.plan_done` delante —imprime un informe y termina—, así que
    // hasta 0.42.0 esta línea no se podía escribir: cinco copias contra un
    // destino sin papelera y cinco contra uno con papelera restaurable eran
    // byte a byte el mismo informe. Solo cuando algo se aplicó: decirle «nada
    // se puede deshacer» a quien no hizo nada es ruido.
    if report.done > 0 {
        let outlook = norte_frontend::sync::UndoOutlook::of_report(report);
        println!(
            "{}",
            norte_i18n::t(&format!("sync-outlook-{}", outlook.id()))
        );
    }
    // Una fila de fallo son TRES campos y va en TRES líneas, no en una unida
    // por `: ` y ` → ` (auditoría de encoding MAJOR-4). Los dos joiners son
    // imprimibles corrientes que `display_name_with` no enmascara, así que
    // llegan SIN el `!` de `marcado`: `informe :→ copia.txt: permission
    // denied` es un nombre legal en ext4 y APFS —está en la corpus, como
    // `cause_join_spoof`— y en banda imprimía una fila entera fabricada,
    // después de un `Mirror` destructivo.
    //
    // El salto de línea SÍ es un separador que un nombre no puede falsificar:
    // `\n` es Cc, `is_terminal_hazard` lo enmascara a `U+FFFD` y el nombre
    // llega badgeado. Es lo que la GUI consigue con elementos hermanos y una
    // tubería no tiene.
    //
    // Esta lista ya se enseña también tras una cancelación (#187): el
    // `match` de arriba solo AVISA de cómo acabó, y el informe —éste, con sus
    // fallos— se pide y se imprime igual sea cual sea el desenlace terminal.
    for failure in &report.failures {
        // Por `render_failure` y no por dos `rel_display` sueltos: el plegado
        // de la ortografía del destino cuando los BYTES coinciden es la misma
        // regla que la de un paso, y vive una sola vez para los tres frontends
        // (#161). Repetir la misma ruta con una flecha en medio sugiere un
        // renombrado que no hay.
        let cells = norte_frontend::sync::render_failure(
            failure,
            norte_frontend::sync::SyncEncodings::default(),
        );
        eprintln!(
            "{}",
            norte_i18n::ta("cli-sync-failure", &[("rel", &rel_marcado(&cells.rel))])
        );
        // El ancla, por la misma razón que en el plan: `render_failure` la
        // calcula y esta llamada existe para ella, pero la CLI la tiraba
        // (auditoría de encoding, MAJOR-1). Un `DeleteTree` denegado bajo
        // `Mirror` es la fila hostil más común de un `Mirror`, y su `rel`
        // cuelga del DESTINO: sin calificar, el operador va a arreglar el
        // árbol equivocado.
        if let Some(q) = norte_frontend::sync::anchor_label(cells.anchor, norte_i18n::active()) {
            eprintln!("  {q}");
        }
        if let Some(d) = &cells.dest_rel {
            eprintln!(
                "  {}",
                norte_i18n::ta("cli-sync-failure-dest", &[("dest", &rel_marcado(d))])
            );
            // #192: sin badge en ninguna mitad (las dos son UTF-8 válido), un
            // par NFC/NFD se repite en dos líneas sin nada que lo explique.
            if let Some(q) =
                norte_frontend::sync::dest_twin_label(cells.dest_rel_twin, norte_i18n::active())
            {
                eprintln!("  {q}");
            }
        }
        eprintln!(
            "  {}",
            norte_i18n::ta(
                "cli-sync-failure-cause",
                &[(
                    "cause",
                    &norte_frontend::sync::failure_cause_label(failure.cause, norte_i18n::active(),),
                )],
            )
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cli-sync-blocker` no vuelve a unir la ruta y el motivo con `: `
    /// (#189): la fixture `cause_join_spoof` de la corpus llevaba justo ese
    /// joiner y habría fingido una fila entera.
    #[test]
    fn el_bloqueo_no_une_ruta_y_motivo_en_una_linea() {
        for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
            let rel_line = norte_i18n::ta_in(lang, "cli-sync-blocker", &[("rel", "sub/a.txt")]);
            assert_eq!(rel_line, "sub/a.txt", "{lang:?}: nada pegado a la ruta");
            let why_line =
                norte_i18n::ta_in(lang, "cli-sync-blocker-why", &[("why", "dest read only")]);
            assert!(why_line.contains("dest read only"), "{lang:?}: {why_line}");
            assert!(!why_line.contains("sub/a.txt"), "{lang:?}: {why_line}");
        }
    }

    /// Un bloqueo de todo el árbol (`DestReadOnly`, cuyo `rel` es la raíz) no
    /// se pinta como una ruta vacía (#193): `report_blockers` usa
    /// `rel_display_or_root`, no `rel_display` a secas, precisamente para
    /// esto.
    #[test]
    fn un_bloqueo_de_todo_el_arbol_no_imprime_una_ruta_vacia() {
        let root = norte_proto::methods::RelPath::parse_wire("").expect("rel");
        assert!(root.is_root());
        let blocker = norte_proto::methods::SyncBlocker {
            rel: root.clone(),
            kind: norte_proto::methods::SyncBlockerKind::DestReadOnly,
            side: None,
        };
        let anchor = norte_frontend::sync::blocker_anchor(&blocker);
        let rel = norte_frontend::sync::rel_display_or_root(&root, None, norte_i18n::Lang::En);
        assert!(!rel.text.is_empty(), "la raíz no se pinta como nada");
        assert_eq!(anchor, norte_frontend::sync::RelAnchor::Dest);
    }

    /// #189, con un nombre ADVERSARIAL: `cause_join_spoof`
    /// (`informe :→ copia.txt: permission denied`) lleva los DOS joiners que
    /// una fila de bloqueo en banda fabricaría (` → ` y `: `), y es
    /// imprimible corriente —`display_name_with` no lo enmascara, así que
    /// `rel_marcado` no lo marca—. La prueba tibia de arriba solo cubre la
    /// plantilla con literales inocuos; ésta hace pasar el nombre REAL por
    /// el mismo camino que `report_blockers` usa (`rel_display_or_root` +
    /// `rel_marcado`), que es donde una fila fabricada tendría que aparecer
    /// si alguien reintrodujera el joiner.
    #[test]
    fn un_bloqueo_con_un_nombre_adversarial_no_fabrica_una_fila() {
        let fixture = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|f| f.id == "cause_join_spoof")
            .expect("corpus");
        let blocker = norte_proto::methods::SyncBlocker {
            rel: norte_proto::methods::RelPath::new(vec![
                norte_proto::Segment::new(fixture.bytes.clone()).expect("seg"),
            ]),
            kind: norte_proto::methods::SyncBlockerKind::DestReadOnly,
            side: None,
        };
        let lang = norte_i18n::Lang::En;
        let anchor = norte_frontend::sync::blocker_anchor(&blocker);
        let rel = norte_frontend::sync::rel_display_or_root(
            &blocker.rel,
            norte_frontend::sync::SyncEncodings::default().for_anchor(anchor),
            lang,
        );
        let rel_line = rel_marcado(&rel);
        let why_line = norte_frontend::sync::blocker_label(blocker.kind, lang);
        assert!(
            !rel_line.contains(&why_line),
            "la línea de la ruta no lleva pegado el motivo: {rel_line:?}"
        );
        assert!(
            !why_line.contains("permission denied"),
            "la línea del motivo no lleva pegados los bytes del nombre: {why_line:?}"
        );
        // Y el propio joiner que el fixture lleva DENTRO del nombre no se
        // confunde con uno estructural: sigue siendo parte del texto pintado.
        assert!(rel_line.contains("permission denied"), "{rel_line:?}");
    }

    /// `report_blockers` no panica para ninguna combinación de clase y lado,
    /// y siempre devuelve el 2 —nada se aplicó— sea cual sea el bloqueo.
    #[test]
    fn report_blockers_no_panica_para_cualquier_clase_o_lado() {
        use norte_proto::methods::{
            DestTrash, PlanHash, Side, SyncBlocker, SyncBlockerKind, SyncCounts, SyncPlanDone,
        };
        for kind in [
            SyncBlockerKind::AmbiguousDest,
            SyncBlockerKind::OverlapDetected,
            SyncBlockerKind::DestReadOnly,
            SyncBlockerKind::DirTooLarge,
            SyncBlockerKind::TypeMismatchDir,
            SyncBlockerKind::Unknown,
        ] {
            for side in [None, Some(Side::Left), Some(Side::Right)] {
                let done = SyncPlanDone {
                    task_id: norte_proto::TaskId::new(1),
                    plan_hash: PlanHash::parse(&"a".repeat(64)).expect("hex"),
                    counts: SyncCounts::default(),
                    blockers: vec![SyncBlocker {
                        rel: norte_proto::methods::RelPath::parse_wire("sub/a.txt").expect("rel"),
                        kind,
                        side,
                    }],
                    blockers_total: 1,
                    executable: false,
                    dest_trash: DestTrash::Restorable,
                };
                assert_eq!(
                    report_blockers(&done),
                    ExitCode::from(2),
                    "{kind:?}/{side:?}"
                );
            }
        }
    }
}

#[cfg(test)]
mod plan_step_lines_tests {
    use super::plan_step_lines;

    fn seg(b: &[u8]) -> norte_proto::methods::RelPath {
        norte_proto::methods::RelPath::new(vec![
            norte_proto::Segment::new(b.to_vec()).expect("segmento"),
        ])
    }

    /// La fila del plan con las DOS ortografías: cada campo en su línea.
    ///
    /// Antes iban unidas por ` → ` en la misma línea, y ese carácter es un
    /// imprimible corriente que `display_name_with` no enmascara — o sea que
    /// un nombre que lo lleve dentro (corpus `arrow_join_spoof`) fingía la
    /// pareja SIN que saltara el `!` de `rel_marcado`. Esto es la pantalla
    /// donde se teclea `y` para borrar.
    #[test]
    fn las_dos_ortografias_no_comparten_linea() {
        let paso = norte_proto::methods::SyncStep {
            id: 1,
            kind: norte_proto::methods::SyncStepKind::Overwrite,
            rel: seg(b"a \xe2\x86\x92 mem_b.txt"),
            dest_rel: Some(seg(b"otro.txt")),
            size: Some(10),
            criterion: norte_proto::methods::CompareCriterion::Size,
            confidence: norte_proto::methods::CompareConfidence::Certain,
            reversal: Some(norte_proto::methods::StepReversal::Delete),
            reason: None,
        };
        let cells = norte_frontend::sync::render_step(
            &paso,
            norte_proto::methods::DestTrash::Restorable,
            norte_frontend::sync::SyncEncodings::default(),
        );
        let lineas = plan_step_lines(&cells);
        let primera = &lineas[0];
        assert!(
            primera.contains("mem_b.txt"),
            "el nombre del origen va entero: {primera:?}"
        );
        assert!(
            !primera.contains("otro.txt"),
            "la ortografía del DESTINO no comparte línea con el nombre: {primera:?}"
        );
        assert!(
            lineas.iter().skip(1).any(|l| l.contains("otro.txt")),
            "pero sí se dice, en su propia línea: {lineas:?}"
        );
    }

    /// Y el ancla se PINTA. `render_failure`/`render_step` la calculan y la
    /// CLI era el único painter de los tres que la tiraba: en una lista donde
    /// una ruta sin calificar significa «del origen», callar un `Dest` lo
    /// afirma — y el `rel` de un `DeleteTree` cuelga del destino.
    #[test]
    fn un_delete_tree_dice_que_su_ruta_es_del_destino() {
        let paso = norte_proto::methods::SyncStep {
            id: 2,
            kind: norte_proto::methods::SyncStepKind::DeleteTree,
            rel: seg(b"viejo"),
            dest_rel: None,
            size: None,
            criterion: norte_proto::methods::CompareCriterion::Presence,
            confidence: norte_proto::methods::CompareConfidence::Certain,
            reversal: Some(norte_proto::methods::StepReversal::RestoreTrash),
            reason: None,
        };
        let cells = norte_frontend::sync::render_step(
            &paso,
            norte_proto::methods::DestTrash::Restorable,
            norte_frontend::sync::SyncEncodings::default(),
        );
        assert_eq!(cells.anchor, norte_frontend::sync::RelAnchor::Dest);
        let lineas = plan_step_lines(&cells);
        let esperado = norte_frontend::sync::anchor_label(cells.anchor, norte_i18n::active())
            .expect("Dest tiene calificador");
        assert!(
            lineas.iter().skip(1).any(|l| l.contains(&esperado)),
            "el calificador del ancla se pinta: {lineas:?}"
        );
    }

    /// Un par NFC/NFD (#192) pinta la misma cadena en las dos líneas de
    /// ortografía, y sin la nota el lector no tiene forma de distinguir eso
    /// de un renombrado que no hizo nada.
    #[test]
    fn un_par_nfc_nfd_lleva_su_propia_nota() {
        let fixtures = norte_testkit::corpus::hostile_names();
        let nfc = fixtures
            .iter()
            .find(|f| f.id == "nfc_e_acute")
            .expect("corpus");
        let nfd = fixtures
            .iter()
            .find(|f| f.id == "nfd_e_acute")
            .expect("corpus");
        let paso = norte_proto::methods::SyncStep {
            id: 3,
            kind: norte_proto::methods::SyncStepKind::Overwrite,
            rel: seg(&nfc.bytes),
            dest_rel: Some(seg(&nfd.bytes)),
            size: Some(1),
            criterion: norte_proto::methods::CompareCriterion::Size,
            confidence: norte_proto::methods::CompareConfidence::Certain,
            reversal: Some(norte_proto::methods::StepReversal::Delete),
            reason: None,
        };
        let cells = norte_frontend::sync::render_step(
            &paso,
            norte_proto::methods::DestTrash::Restorable,
            norte_frontend::sync::SyncEncodings::default(),
        );
        assert!(cells.dest_rel_twin);
        let lineas = plan_step_lines(&cells);
        let esperado = norte_frontend::sync::dest_twin_label(true, norte_i18n::active())
            .expect("hay nota cuando twin es true");
        assert!(
            lineas.iter().any(|l| l.contains(&esperado)),
            "la nota se pinta: {lineas:?}"
        );
    }
}
