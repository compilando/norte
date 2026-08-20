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
use crate::jobs::{InFlight, PendingAiPlan, RenameBatchRun};

/// Lo que devuelve una petición spawneada: el error del backend por dentro,
/// el del `join` por fuera (aborto por Esc, o pánico del future).
type Harvested<T> = Result<Result<T, Error>, tokio::task::JoinError>;

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
                let state = if let Some(pairs) = norte_frontend::rename_pairs(&plan.entries) {
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
                    entries: plan.entries,
                    plan: state,
                };
                if app.modal.is_none() {
                    app.modal = Some(Modal::AiRenamePlan {
                        dir: ready.dir,
                        entries: ready.entries,
                        offset: 0,
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
