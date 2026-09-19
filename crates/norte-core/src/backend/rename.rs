//! El área de renombrado y organización de [`Backend`](super::Backend): el
//! lote revisable de `fs.rename_batch*`, y los planes de IA `ai.rename_plan`
//! / `ai.organize_plan` con su aplicación (`organize`).

use norte_proto::{Error, TaskId, VPath};

use super::{AI_CALL_TIMEOUT, Backend, TaskRef};

impl Backend {
    /// El plan REVISABLE de un lote de renames dentro de `dir` (spec §17, ADR
    /// 0042). NO muta nada: ni Task, ni journal.
    ///
    /// Lo que se manda es INTENCIÓN — parejas de nombres base. El orden, los
    /// temporales y los veredictos los decide el core (regla dura 7), y el
    /// `plan_hash` que vuelve es el que hay que devolver a
    /// [`Backend::rename_batch`] para ejecutar EXACTAMENTE lo que se enseñó.
    ///
    /// # Errors
    /// [`Error::InvalidPath`] si un nombre no es una entrada de directorio
    /// legal o si hay más de `FS_RENAME_BATCH_MAX_PAIRS` parejas;
    /// [`Error::Unsupported`] sin provider o con uno de solo lectura;
    /// [`Error::LimitExceeded`] en un directorio inabarcable;
    /// [`Error::PolicyDenied`] del gate de lectura (remoto, agente sin scope);
    /// taxonomía del protocolo.
    pub async fn rename_batch_plan(
        &self,
        dir: &VPath,
        pairs: &[norte_proto::methods::RenamePair],
    ) -> Result<norte_proto::methods::FsRenameBatchPlanResult, Error> {
        match self {
            Self::Embedded(engine) => {
                let raw = crate::rename::pairs_from_wire(pairs);
                let plan = engine.rename_batch_plan(dir, &raw).await?;
                crate::rename::plan_to_proto(&plan)
            }
            #[cfg(unix)]
            Self::Remote(r) => r.rename_batch_plan(dir, pairs).await,
        }
    }

    /// Ejecuta el lote aprobado como UNA Task y UNA unidad deshacible del
    /// journal (spec §17, ADR 0042).
    ///
    /// `plan_hash` es el token de FRESCURA de [`Backend::rename_batch_plan`],
    /// atado al directorio. El core re-planifica el directorio TAL COMO ESTÁ
    /// AHORA y compara: si derivó, esto es [`Error::PlanStale`] y no se toca
    /// nada. No es una prueba de aprobación —el digest es público y calculable
    /// sin haber pedido el plan—: garantiza QUÉ se ejecuta, no que alguien lo
    /// mirara. El informe de lo que
    /// pasó se pide con [`Backend::rename_batch_report`] — la Task terminal
    /// cuenta la causa, no lo que se quedó a medias.
    ///
    /// # Errors
    /// [`Error::PlanStale`] si el directorio derivó desde el plan;
    /// [`Error::PlanNotExecutable`] si el plan aprobado tenía colisiones;
    /// [`Error::PolicyDenied`] del gate de mutación; más las de
    /// [`Backend::rename_batch_plan`].
    pub async fn rename_batch(
        &self,
        dir: &VPath,
        pairs: &[norte_proto::methods::RenamePair],
        plan_hash: &norte_proto::methods::PlanHash,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let raw = crate::rename::pairs_from_wire(pairs);
                let (handle, _report) = engine.rename_batch(dir, &raw, plan_hash).await?;
                // El informe queda en el anillo del engine, que es de donde lo
                // lee `rename_batch_report`: los dos brazos se piden igual.
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r
                .rename_batch(dir, pairs, plan_hash)
                .await
                .map(TaskRef::from),
        }
    }

    /// El informe de un lote ya lanzado (`fs.rename_batch_report`, 0.36.0):
    /// cuántos pasos se aplicaron, cuántos se deshicieron y —lo que ningún
    /// error pelado puede decir— QUÉ paso se quedó aplicado y bajo qué nombre.
    ///
    /// Míralo también cuando la Task diga `cancelled`: cancelar un lote lo
    /// deshace, y un rollback también puede atascarse.
    ///
    /// # Errors
    /// [`Error::NotFound`] si ese `task_id` nunca fue un lote de este proceso
    /// o si el anillo ya lo desalojó — y el brazo remoto contesta lo MISMO,
    /// porque el daemon manda esa categoría y no un `-32602` sin taxonomía;
    /// [`Error::Unsupported`] contra un daemon N-1 que no conoce el método;
    /// taxonomía del protocolo.
    pub async fn rename_batch_report(
        &self,
        task_id: TaskId,
    ) -> Result<norte_proto::methods::FsRenameBatchReportResult, Error> {
        match self {
            Self::Embedded(engine) => engine
                .rename_batch_report(task_id)
                .map(|(_owner, r)| crate::rename::report_to_proto(&r))
                // Embebido no hay actor que comprobar: este `Backend` ES el
                // humano en proceso (mismo criterio que
                // `plugins_set_approval`).
                .ok_or(Error::NotFound),
            #[cfg(unix)]
            Self::Remote(r) => r.rename_batch_report(task_id).await,
        }
    }

    /// Plan de rename revisable de `dir` vía IA (M4-IA, ADR 0031). NO muta:
    /// aplicar el plan son N [`Backend::move_`] gobernados. AMBOS brazos
    /// están acotados por `AI_CALL_TIMEOUT` (2 min): un endpoint de proveedor en
    /// dead-air jamás cuelga el frontend embebido ni el remoto.
    ///
    /// # Errors
    /// [`Error::Unsupported`] sin proveedor de IA; [`Error::PolicyDenied`]
    /// del gate de IA (off, local-only, denied prefix);
    /// [`Error::ProviderUnavailable`] (retryable) al agotar el timeout;
    /// taxonomía del protocolo para fallos del proveedor.
    /// `names` son los basenames MARCADOS (#121). Vacío = el directorio
    /// entero, que es lo que este método hacía: con la selección de primera
    /// clase, pedir un plan sobre cinco ficheros mandaba los mil del
    /// directorio al proveedor.
    pub async fn ai_rename_plan(
        &self,
        dir: &VPath,
        instruction: &str,
        names: &[String],
    ) -> Result<norte_proto::methods::AiRenamePlanResult, Error> {
        match self {
            Self::Embedded(engine) => {
                let plan = tokio::time::timeout(
                    AI_CALL_TIMEOUT,
                    engine.ai_rename_plan_for(dir, instruction, names),
                )
                .await
                .map_err(|_| Error::ProviderUnavailable { retryable: true })??;
                Ok(crate::ai::ai_plan_to_proto(plan))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.ai_rename_plan(dir, instruction, names).await,
        }
    }

    /// Plan de ORGANIZAR por IA (0.77.0, fase 8): revisable, no muta nada.
    ///
    /// # Errors
    /// `Unsupported` sin proveedor; el gate de IA con su motivo; la taxonomía
    /// del protocolo. `ProviderUnavailable` al agotar el timeout, como su
    /// hermano.
    pub async fn ai_organize_plan(
        &self,
        dir: &VPath,
        instruction: &str,
        names: &[String],
    ) -> Result<norte_proto::methods::AiOrganizePlanResult, Error> {
        match self {
            Self::Embedded(engine) => {
                let plan = tokio::time::timeout(
                    AI_CALL_TIMEOUT,
                    engine.ai_organize_plan_for(dir, instruction, names),
                )
                .await
                .map_err(|_| Error::ProviderUnavailable { retryable: true })??;
                // El token viaja CON el plan (ver `organize::plan_hash`): sin
                // él el modal abriría sobre algo que no se puede aprobar.
                let plan_hash = if plan.moves.is_empty() {
                    None
                } else {
                    Some(crate::organize::plan_hash(dir, &plan.moves)?)
                };
                Ok(norte_proto::methods::AiOrganizePlanResult {
                    moves: plan.moves,
                    refused: None,
                    plan_hash,
                })
            }
            #[cfg(unix)]
            Self::Remote(r) => r.ai_organize_plan(dir, instruction, names).await,
        }
    }

    /// Aplica un plan de organizar (0.77.0, fase 8): crea las carpetas y
    /// mueve, como UN lote deshacible.
    ///
    /// # Errors
    /// `PlanStale` si el token no es el del plan revisado; `InvalidPath` si
    /// algún destino se sale del directorio; la taxonomía del protocolo.
    pub async fn organize(
        &self,
        dir: &VPath,
        moves: &[norte_proto::methods::OrganizeMove],
        plan_hash: &norte_proto::methods::PlanHash,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let handle = engine
                    .organize(dir, moves, plan_hash, crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.organize(dir, moves, plan_hash).await.map(TaskRef::from),
        }
    }
}
