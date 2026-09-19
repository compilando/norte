//! El área de journal de [`Backend`](super::Backend): decidir aprobaciones,
//! deshacer (sesión de agente o por `seq`), y la línea de tiempo `journal.list`.

use norte_proto::{Error, TaskId};

use super::{Backend, TaskRef};

impl Backend {
    /// Resuelve una aprobación pendiente (`policy.decide`, M3-3b T5).
    ///
    /// # Errors
    /// Taxonomía del protocolo; `Unsupported` en embebido (las aprobaciones
    /// solo llegan por el canal del daemon, así que aquí no hay qué decidir).
    pub async fn policy_decide(&self, approval_id: u64, approve: bool) -> Result<(), Error> {
        match self {
            Self::Embedded(_) => Err(Error::Unsupported),
            #[cfg(unix)]
            Self::Remote(r) => r.policy_decide(approval_id, approve).await,
        }
    }

    /// Deshace la sesión completa de un agente como Task (`policy.undo_session`,
    /// M3-4). Solo tiene sentido contra el daemon (dueño del journal).
    ///
    /// # Errors
    /// Taxonomía del protocolo; `Unsupported` en embebido. Desde #167 el
    /// embebido sí puede tener journal, pero deshacer LA SESIÓN DE UN AGENTE es
    /// del daemon: los agentes se gobiernan ahí y es ahí donde existen sus
    /// sesiones.
    pub async fn undo_session(&self, session: &str) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(_) => Err(Error::Unsupported),
            #[cfg(unix)]
            Self::Remote(r) => r.undo_session(session).await.map(TaskRef::from),
        }
    }

    /// Una página de la línea de tiempo del journal (`journal.list`, 0.76.0,
    /// fase 7): de la más nueva hacia atrás, `before_seq` exclusivo.
    ///
    /// Funciona EMBEBIDO, al contrario que [`Backend::undo_session`]: lo que
    /// aquél no puede contestar sin daemon son las sesiones de agente, y esto
    /// es el journal de esta máquina, que el engine embebido tiene delante.
    ///
    /// # Errors
    /// Taxonomía del protocolo. `Unsupported` sin journal; contra un daemon,
    /// `PolicyDenied` si la conexión no es humana.
    pub async fn journal_list(
        &self,
        before_seq: Option<i64>,
        limit: u32,
        actor_kind: Option<&str>,
    ) -> Result<norte_proto::methods::JournalListResult, Error> {
        match self {
            Self::Embedded(engine) => {
                let limit = limit.clamp(1, norte_proto::methods::JOURNAL_LIST_MAX_PAGE);
                let entries = engine.journal_page(before_seq, limit, actor_kind).await?;
                // El MISMO cálculo de cursor que el daemon, porque es la
                // misma función: dos copias de esta expresión es lo que la
                // revisión de protocolo señaló, y ninguna podía ponerse roja
                // por su cuenta.
                Ok(crate::journal::page_to_wire(&entries, limit))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.journal_list(before_seq, limit, actor_kind).await,
        }
    }

    /// Deshace lo que el humano hizo DESPUÉS de `seq` (`journal.undo_after`,
    /// 0.76.0, fase 7). La entrada señalada se queda.
    ///
    /// También embebido, por lo mismo que [`Backend::journal_list`]: deshacer
    /// lo propio no necesita daemon. El informe se lee como el de cualquier
    /// undo.
    ///
    /// # Errors
    /// Taxonomía del protocolo; `Unsupported` sin journal.
    pub async fn undo_after(&self, seq: i64) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let (handle, _report) = engine.undo_after(seq).await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.undo_after(seq).await.map(TaskRef::from),
        }
    }

    /// Informe de una Task de undo (`policy.undo_report`, #71): qué se
    /// deshizo, qué se saltó y por qué, dónde se bloqueó el LIFO. Snapshot;
    /// definitivo cuando la Task es terminal.
    ///
    /// # Errors
    /// Taxonomía del protocolo; [`Error::NotFound`], por los dos brazos (el
    /// daemon desde 0.79.0), si ese id no fue un undo o el anillo ya lo
    /// desalojó.
    pub async fn undo_report(
        &self,
        task_id: TaskId,
    ) -> Result<norte_proto::methods::PolicyUndoReportResult, Error> {
        match self {
            // Embebido no hay actor que comprobar: este `Backend` ES el humano
            // en proceso (mismo criterio que `rename_batch_report`).
            Self::Embedded(engine) => engine
                .undo_report(task_id)
                .map(|(_owner, r)| crate::undo::report_to_proto(r))
                .ok_or(Error::NotFound),
            #[cfg(unix)]
            Self::Remote(r) => r.undo_report(task_id).await,
        }
    }
}
