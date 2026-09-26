//! [`Backend`](super::Backend)'s index area (M4): building, querying,
//! generating embeddings and searching semantically.

use norte_proto::{Error, VPath};

use super::{AI_CALL_TIMEOUT, Backend, TaskRef, index_hit_to_proto};

impl Backend {
    /// (Re)builds `root`'s index as a Task (M4, ADR 0034).
    ///
    /// # Errors
    /// [`Error::Unsupported`] with no index; protocol taxonomy.
    pub async fn index_build(&self, root: &VPath) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let (handle, _report) = engine
                    .index_build_as(root.clone(), crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            Self::Remote(r) => r.index_build(root).await.map(TaskRef::from),
        }
    }

    /// Queries `root`'s index for `text` (M4). Returns protocol hits.
    ///
    /// # Errors
    /// [`Error::Unsupported`] with no index; protocol taxonomy.
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
            Self::Remote(r) => r.index_query(root, text, limit).await,
        }
    }

    /// Generates embeddings for `root`'s already-indexed files as a Task
    /// (M4-IA-2). Requires a prior `index.build` of the SAME root.
    ///
    /// # Errors
    /// [`Error::Unsupported`] with no index or no embeddings provider;
    /// [`Error::NotFound`] with no prior `index.build` (in the RESPONSE, not
    /// the join); [`Error::PolicyDenied`] from the AI gate; protocol
    /// taxonomy.
    pub async fn index_embed(&self, root: &VPath) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let handle = engine
                    .index_embed_as(root.clone(), crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            Self::Remote(r) => r.index_embed(root).await.map(TaskRef::from),
        }
    }

    /// Semantic search over the index's embeddings (M4-IA-2): `root = None`
    /// searches every root. BOTH arms are bounded by `AI_CALL_TIMEOUT` (the
    /// query's embed goes to the provider), like [`Backend::ai_rename_plan`].
    ///
    /// # Errors
    /// [`Error::Unsupported`] with no index or no embeddings provider;
    /// [`Error::PolicyDenied`] from the AI gate; [`Error::ProviderUnavailable`]
    /// (retryable) when the timeout runs out; protocol taxonomy.
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
            Self::Remote(r) => r.index_search_semantic(root, query, k).await,
        }
    }
}
