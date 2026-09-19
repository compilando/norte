//! El área de índice de [`Backend`](super::Backend) (M4): construir,
//! consultar, generar embeddings y buscar semánticamente.

use norte_proto::{Error, VPath};

use super::{AI_CALL_TIMEOUT, Backend, TaskRef, index_hit_to_proto};

impl Backend {
    /// (Re)construye el índice de `root` como Task (M4, ADR 0034).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay índice; taxonomía del protocolo.
    pub async fn index_build(&self, root: &VPath) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let (handle, _report) = engine
                    .index_build_as(root.clone(), crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.index_build(root).await.map(TaskRef::from),
        }
    }

    /// Consulta el índice de `root` por `text` (M4). Devuelve hits del protocolo.
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay índice; taxonomía del protocolo.
    pub async fn index_query(
        &self,
        root: &VPath,
        text: &str,
        limit: u32,
    ) -> Result<Vec<norte_proto::methods::IndexHit>, Error> {
        match self {
            Self::Embedded(engine) => {
                let hits = engine
                    .index_query_as(root, text, limit, crate::journal::Actor::User)
                    .await?;
                Ok(hits.into_iter().map(index_hit_to_proto).collect())
            }
            #[cfg(unix)]
            Self::Remote(r) => r.index_query(root, text, limit).await,
        }
    }

    /// Genera embeddings de los ficheros ya indexados de `root` como Task
    /// (M4-IA-2). Requiere `index.build` previo del MISMO root.
    ///
    /// # Errors
    /// [`Error::Unsupported`] sin índice o sin proveedor de embeddings;
    /// [`Error::NotFound`] sin `index.build` previo (en la RESPUESTA, no en
    /// el join); [`Error::PolicyDenied`] del gate de IA; taxonomía del
    /// protocolo.
    pub async fn index_embed(&self, root: &VPath) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let handle = engine
                    .index_embed_as(root.clone(), crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.index_embed(root).await.map(TaskRef::from),
        }
    }

    /// Búsqueda semántica sobre los embeddings del índice (M4-IA-2):
    /// `root = None` busca en todos los roots. AMBOS brazos acotados por
    /// `AI_CALL_TIMEOUT` (el embed de la query va al proveedor), como
    /// [`Backend::ai_rename_plan`].
    ///
    /// # Errors
    /// [`Error::Unsupported`] sin índice o sin proveedor de embeddings;
    /// [`Error::PolicyDenied`] del gate de IA;
    /// [`Error::ProviderUnavailable`] (retryable) al agotar el timeout;
    /// taxonomía del protocolo.
    pub async fn index_search_semantic(
        &self,
        root: Option<&VPath>,
        query: &str,
        k: u32,
    ) -> Result<Vec<norte_proto::methods::SemanticHit>, Error> {
        match self {
            Self::Embedded(engine) => {
                let hits = tokio::time::timeout(
                    AI_CALL_TIMEOUT,
                    engine.index_search_semantic(root, query, k),
                )
                .await
                .map_err(|_| Error::ProviderUnavailable { retryable: true })??;
                Ok(hits
                    .into_iter()
                    .map(|(path, score)| norte_proto::methods::SemanticHit { path, score })
                    .collect())
            }
            #[cfg(unix)]
            Self::Remote(r) => r.index_search_semantic(root, query, k).await,
        }
    }
}
