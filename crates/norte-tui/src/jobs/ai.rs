//! Las tres peticiones LARGAS de la IA y del lote, cosechadas sin bloquear.
//!
//! Las tres tienen la forma de las tareas de panel de al lado: se spawnean,
//! se cosechan en un brazo del `select!` y a lo sumo hay una viva —relanzar
//! aborta la anterior—. Lo que las distingue es qué hacen con la respuesta, y
//! eso es lo que vive aquí: los cinturones de ingestión (un plan por encima
//! del tope o una pareja que no es un `Segment` delatan un daemon hostil, y
//! se rechazan en bloque) y la disciplina de no pisar jamás un modal abierto.

use norte_core::backend::Backend;
use norte_i18n::{t, ta};
use norte_proto::Error;

use crate::app::{App, Modal, detail_for_bar, error_category};
use crate::jobs::{AiRenameRun, InFlight, PendingAiPlan, RenameBatchRun};

/// C3 (ADR 0095): pide su plan a un plugin `renamer` sobre lo marcado —o lo
/// señalado— y lo deja en el MISMO run que el plan de la IA: el cosechado
/// no distingue quién lo propuso, y por eso no hay un segundo camino de
/// revisión. El operando es el del lote por plantilla
/// (`rename_batch_names`): solo nombres que son texto, porque un par viaja
/// UTF-8.
pub fn spawn_renamer_plan(
    app: &mut App,
    backend: &Backend,
    work: &mut InFlight,
    id: &str,
    renamer: &str,
) {
    let names = app.rename_batch_names();
    if names.is_empty() {
        app.message = Some(t("msg-rename-batch-nothing"));
        return;
    }
    let dir = app.focused().dir().clone();
    let b = backend.clone();
    let d = dir.clone();
    let (id, renamer) = (id.to_owned(), renamer.to_owned());
    let handle = tokio::spawn(async move { b.plugin_rename_plan(&id, &renamer, &d, &names).await });
    // Los nombres del directorio que se PLANEA (#275): el cinturón del
    // cosechado exige que cada `from` exista donde se va a aplicar.
    let del_dir: Vec<Vec<u8>> = app
        .focused()
        .entries()
        .iter()
        .filter_map(|e| e.path.file_name().map(|s| s.as_bytes().to_vec()))
        .collect();
    let run = AiRenameRun {
        handle,
        dir,
        names: del_dir,
    };
    if let Some(old) = work.ai_rename.replace(run) {
        old.handle.abort();
    }
    work.pending_ai_plan = None;
    app.message = Some(t("msg-ai-rename-running"));
}

/// Lo que devuelve una petición spawneada: el error del backend por dentro,
/// el del `join` por fuera (aborto por Esc, o pánico del future).
type Harvested<T> = Result<Result<T, Error>, tokio::task::JoinError>;

/// El informe de un lote de sumas (#311): abre el modal con lo calculado, y con
/// el VEREDICTO si esto era una comprobación.
///
/// Un informe que no llega —la Task falló, o el anillo ya lo desalojó— sale por
/// la barra y no abre nada: un modal vacío haría creer que se comprobó algo.
pub fn harvest_checksum(
    app: &mut App,
    work: &mut InFlight,
    res: Result<
        (
            norte_proto::TaskState,
            Result<norte_proto::methods::FsChecksumReportResult, Error>,
        ),
        tokio::task::JoinError,
    >,
) {
    use norte_frontend::checksums;

    let Some(run) = work.checksum.take() else {
        return;
    };
    let (estado, informe) = match res {
        Ok(par) => par,
        // El aborto de un relevo NO llega aquí: al relevar, el handle viejo se
        // aborta Y se dropea, y el brazo del `select!` ya solo poll-ea el
        // nuevo. Lo que sí llega es un pánico del future, y ahí no hay nadie
        // que haya puesto mensaje: sin esto la barra se quedaba en «calculando
        // sumas…» para siempre y no se abría nada.
        Err(join) => {
            if join.is_panic() {
                app.message = Some(t("msg-checksum-failed"));
            }
            return;
        }
    };
    let informe = match informe {
        Ok(r) => r,
        Err(e) => {
            app.message = Some(crate::app::error_message(&e));
            return;
        }
    };
    // Un informe de una Task que se CANCELÓ o que falló está a medias, y
    // `pending > 0` lo dice. Pintar veredictos sobre él acusaría —«no cuadra o
    // falta»— a ficheros que nadie llegó a leer, que es el peor error posible
    // en la única herramienta cuyo trabajo es comprobar. El estado de la Task
    // ya se dijo por la barra (cancelado / el error), así que aquí basta con
    // no inventarse una conclusión.
    if estado != norte_proto::TaskState::Completed || informe.pending > 0 {
        app.message = Some(t("msg-checksum-partial"));
        return;
    }
    // El digest y el motivo de cada ruta, EN EL ORDEN PEDIDO, que es como el
    // informe los devuelve y como se emparejan.
    let calculado: Vec<checksums::Computed> = informe
        .entries
        .iter()
        .map(|e| (e.digest.clone(), e.miss))
        .collect();
    // Comprobar: la lista es la del FICHERO DE SUMAS —en su orden y con todas
    // sus líneas, incluidas las que no se pudieron ni pedir— y el veredicto
    // sale del embudo COMPARTIDO, que es donde vive la regla de qué significa
    // cada motivo. Calcular: la lista es lo que se pidió, con su digest.
    let (title_key, rows) = if let Some(publicado) = run.publicado {
        let veredictos = checksums::judge(&publicado.lines, &publicado.asked, &calculado);
        app.message = Some(match checksums::summarize(&veredictos, publicado.refused) {
            // No se puede decir «todos correctos» sobre 37 de 40 líneas: las
            // tres que se cayeron son justo las de los nombres raros.
            checksums::Summary::Unreadable { n, refused } => ta(
                "msg-checksum-unreadable-lines",
                &[("n", &n.to_string()), ("refused", &refused.to_string())],
            ),
            checksums::Summary::AllOk { n } => ta("msg-checksum-all-ok", &[("n", &n.to_string())]),
            checksums::Summary::Bad { n } => ta("msg-checksum-bad", &[("n", &n.to_string())]),
        });
        let rows: Vec<crate::app::ChecksumRow> = publicado
            .lines
            .into_iter()
            .zip(veredictos)
            .map(|(linea, verdict)| crate::app::ChecksumRow {
                name: linea.name,
                digest: None,
                verdict: Some(verdict),
            })
            .collect();
        ("modal-checksums-verify", rows)
    } else {
        // Calcular: el nombre lo pone la ruta pedida, en bytes (regla 1).
        let rows: Vec<crate::app::ChecksumRow> = informe
            .entries
            .iter()
            .map(|e| crate::app::ChecksumRow {
                name: e
                    .path
                    .file_name()
                    .map(|s| s.as_bytes().to_vec())
                    .unwrap_or_default(),
                digest: e.digest.clone(),
                verdict: e.miss.map(|m| match m {
                    norte_proto::methods::ChecksumMiss::NotAFile => checksums::Verdict::NotAFile,
                    _ => checksums::Verdict::Missing,
                }),
            })
            .collect();
        app.message = None;
        ("modal-checksums-create", rows)
    };
    if app.modal.is_none() {
        app.modal = Some(Modal::Checksums {
            title_key,
            rows,
            offset: 0,
        });
    } else {
        // Con otro modal abierto no se pisa nada: las filas se RETIENEN y
        // abren solas al cerrarse el de delante. Tirarlas era peor que no
        // decir nada, porque la barra prometía enseñarlas después.
        work.pending_checksums = Some((title_key, rows));
        app.message = Some(t("msg-checksum-done-hidden"));
    }
}

/// El plan del modelo (M4-IA): abre el modal, o lo RETIENE si hay otro
/// abierto, y de paso pide el plan del LOTE (§17) en el mismo viaje — el
/// modal necesita su `plan_hash` para que confirmar haga algo, y un plan
/// retenido tras otro modal no tendría quién se lo pidiera después.
pub fn harvest_ai_rename(
    app: &mut App,
    backend: &Backend,
    work: &mut InFlight,
    res: Harvested<norte_proto::methods::AiRenamePlanResult>,
) {
    if let Some(run) = work.ai_rename.take() {
        match res {
            // El productor dijo POR QUÉ no propone (#332): un renamer que
            // rehusó. La frase viene ya enmascarada y acotada por el daemon.
            Ok(Ok(norte_proto::methods::AiRenamePlanResult {
                refused: Some(why), ..
            })) => {
                app.message = Some(ta("msg-rename-plan-refused", &[("why", &why)]));
            }
            Ok(Ok(plan)) if plan.entries.is_empty() => {
                app.message = Some(t("msg-ai-rename-empty"));
            }
            // Cinturón de INGESTIÓN (quality review 78eb243
            // MINOR-5): un plan legítimo del engine queda muy
            // por debajo del tope; superarlo delata un daemon
            // hostil/N+1 inflando la respuesta — rechazo en
            // bloque, ni se abre el modal.
            Ok(Ok(plan)) if plan.entries.len() > norte_frontend::MAX_AI_PLAN_ENTRIES => {
                app.message = Some(t("msg-ai-rename-invalid-plan"));
            }
            Ok(Ok(plan)) => {
                // §17: el plan del LOTE se pide AQUÍ, en el mismo
                // viaje que el plan IA — el modal necesita el
                // `plan_hash` para que confirmar haga algo, y un
                // plan retenido tras otro modal no tendría quién
                // se lo pidiera después.
                //
                // SPAWNEADO, como la llamada al modelo: contra un
                // dir enorme o un daemon lento esto es un `fs.list`
                // entero, y esperarlo aquí congelaría el loop —
                // sin dibujo, sin teclas, sin Esc. El modal abre en
                // `Pending` y se rellena solo.
                //
                // Cinturón fail-loud COMPARTIDO con la GUI (audit
                // MAJOR-2): una pareja que no es un `Segment`
                // delata un daemon hostil/roto — ni se le pide
                // plan al core, y confirmar queda muerto.
                // Contra el directorio que se PLANEÓ, no contra el que el
                // pane enseñe ahora (#275).
                let state = if let Some(pairs) =
                    norte_frontend::rename_pairs_in(&plan.entries, Some(&run.names))
                {
                    let b = backend.clone();
                    let d = run.dir.clone();
                    let handle = tokio::spawn(async move { b.rename_batch_plan(&d, &pairs).await });
                    if let Some(old) = work.rename_batch.replace(RenameBatchRun { handle }) {
                        old.handle.abort();
                    }
                    app.message = None;
                    norte_frontend::BatchPlan::Pending
                } else {
                    app.message = Some(t("msg-ai-rename-invalid-plan"));
                    norte_frontend::BatchPlan::Failed
                };
                let ready = PendingAiPlan {
                    dir: run.dir,
                    names: run.names,
                    entries: plan.entries,
                    plan: state,
                };
                if app.modal.is_none() {
                    app.modal = Some(Modal::AiRenamePlan {
                        dir: ready.dir,
                        entries: ready.entries,
                        offset: 0,
                        seen: norte_frontend::AI_RENAME_PAIR_LIMIT,
                        plan: ready.plan,
                    });
                } else {
                    // Otro modal abierto (aprobación, colisión…):
                    // el plan espera su turno, jamás lo pisa. A
                    // diferencia de la GUI (banner superseded), aquí
                    // el overwrite es inalcanzable: run único en
                    // vuelo y el prompt no abre sobre otro modal.
                    work.pending_ai_plan = Some(ready);
                }
            }
            Ok(Err(e)) => {
                app.message = Some(ta(
                    "msg-ai-rename-failed",
                    &[("error", &detail_for_bar(&error_category(&e)))],
                ));
            }
            // Abortado por Esc: silencio, la barra ya se limpió.
            // (Un pánico del future del backend cae aquí también:
            // no hay plan que abrir, el run ya está cosechado.)
            Err(_join) => {}
        }
    }
}

/// El veredicto del lote (§17). El modal puede estar abierto, RETENIDO tras
/// otro, o ya cerrado por el humano: en los dos primeros casos se rellena; en
/// el tercero la respuesta se tira.
pub fn harvest_rename_batch(
    app: &mut App,
    work: &mut InFlight,
    res: Harvested<norte_proto::methods::FsRenameBatchPlanResult>,
) {
    if work.rename_batch.take().is_some() {
        let state = match res {
            Ok(Ok(plan)) => norte_frontend::BatchPlan::Ready(Box::new(plan)),
            Ok(Err(e)) => {
                app.message = Some(ta(
                    "msg-rename-batch-plan-failed",
                    &[("error", &detail_for_bar(&error_category(&e)))],
                ));
                norte_frontend::BatchPlan::Failed
            }
            // Abortado (otra petición lo relevó) o pánico del
            // future: no hay plan y no hay nada más que decir —
            // quien lo relevó ya puso SU mensaje.
            Err(_join) => norte_frontend::BatchPlan::Failed,
        };
        // El modal puede estar abierto, RETENIDO tras otro, o ya
        // cerrado por el humano. En los dos primeros casos se
        // rellena; en el tercero la respuesta se tira.
        if !app.settle_ai_batch_plan(&state)
            && let Some(p) = &mut work.pending_ai_plan
            && p.plan == norte_frontend::BatchPlan::Pending
        {
            p.plan = state;
        }
    }
}

/// Los hits del índice (M4-IA-2): mismo trato que el plan IA —cinturón de
/// ingestión y jamás pisar un modal abierto—.
pub fn harvest_semantic(
    app: &mut App,
    work: &mut InFlight,
    res: Harvested<Vec<norte_proto::methods::SemanticHit>>,
) {
    work.semantic = None;
    match res {
        Ok(Ok(hits)) if hits.is_empty() => {
            app.message = Some(t("msg-semantic-empty"));
        }
        // Cinturón de INGESTIÓN (paridad IA-1): una respuesta
        // por encima del techo contractual del server o con un
        // score no finito delata un daemon hostil/N+1 — rechazo
        // en bloque, ni se abre el modal (el guard es
        // `norte_frontend::validate_semantic_hits`, pura y
        // compartida con la GUI).
        Ok(Ok(hits)) => match norte_frontend::validate_semantic_hits(hits) {
            None => {
                app.message = Some(t("msg-semantic-invalid"));
            }
            Some(hits) => {
                app.message = None;
                if app.modal.is_none() {
                    app.modal = Some(Modal::SemanticHits {
                        hits,
                        offset: 0,
                        cursor: 0,
                    });
                } else {
                    // Otro modal abierto (aprobación, colisión…):
                    // los hits esperan su turno, jamás lo pisan.
                    work.pending_semantic = Some(hits);
                }
            }
        },
        Ok(Err(e)) => {
            app.message = Some(ta(
                "msg-semantic-failed",
                &[("error", &detail_for_bar(&error_category(&e)))],
            ));
        }
        // Abortado por Esc: silencio, la barra ya se limpió.
        // (Un pánico del future del backend cae aquí también:
        // no hay hits que abrir, el run ya está cosechado.)
        Err(_join) => {}
    }
}
