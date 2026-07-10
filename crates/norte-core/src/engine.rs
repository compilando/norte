//! [`Engine`]: la API embebida del core (M0). El daemon JSON-RPC (M1)
//! envolverá esta misma API; los frontends no contienen lógica de negocio.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use norte_proto::{Entry, Error, TaskKind, VPath};
use norte_vfs::{EntryStream, Provider};

use crate::observer::{MutationObserver, NoopObserver};
use crate::ops;
use crate::scheduler::{Priority, Scheduler, TaskHandle};

/// Núcleo embebido: registro de providers por scheme + operaciones.
/// Lecturas (`stat`/`list`) son directas; mutaciones (`copy`/`move_`/
/// `delete`) son Tasks con progreso y cancelación.
pub struct Engine {
    providers: RwLock<HashMap<String, Arc<dyn Provider>>>,
    sched: Scheduler,
    observer: Arc<dyn MutationObserver>,
}

impl Engine {
    /// Engine con el observador no-op (el journal llega en M3).
    #[must_use]
    pub fn new() -> Self {
        Self::with_observer(Arc::new(NoopObserver))
    }

    /// Engine con un observador de mutaciones propio (costura del journal).
    #[must_use]
    pub fn with_observer(observer: Arc<dyn MutationObserver>) -> Self {
        Self {
            providers: RwLock::new(HashMap::new()),
            sched: Scheduler::new(4),
            observer,
        }
    }

    /// Registra un provider bajo su scheme (pisa el anterior si lo había).
    ///
    /// # Panics
    /// Nunca en la práctica: solo por envenenamiento del lock interno.
    pub fn register_provider(&self, provider: Arc<dyn Provider>) {
        let scheme = provider.scheme().to_owned();
        self.providers
            .write()
            .expect("providers lock sano")
            .insert(scheme, provider);
    }

    fn provider_for(&self, p: &VPath) -> Result<Arc<dyn Provider>, Error> {
        self.providers
            .read()
            .expect("providers lock sano")
            .get(p.scheme())
            .cloned()
            .ok_or(Error::Unsupported)
    }

    /// Metadatos de un nodo (directo, sin Task).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay provider para el scheme; los del provider.
    pub async fn stat(&self, p: &VPath) -> Result<Entry, Error> {
        self.provider_for(p)?.stat(p).await
    }

    /// Listado de un directorio (directo, sin Task).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay provider para el scheme; los del provider.
    pub async fn list(&self, p: &VPath) -> Result<EntryStream, Error> {
        self.provider_for(p)?.list(p).await
    }

    /// Copia (recursiva si es dir) como Task.
    ///
    /// # Errors
    /// [`Error::Unsupported`] si algún scheme no tiene provider registrado.
    #[tracing::instrument(skip(self), fields(from = %from.display_lossy(), to = %to.display_lossy()))]
    pub fn copy(&self, from: &VPath, to: &VPath) -> Result<TaskHandle, Error> {
        let src = self.provider_for(from)?;
        let dst = self.provider_for(to)?;
        let observer = Arc::clone(&self.observer);
        let (from, to) = (from.clone(), to.clone());
        let key = to.scheme().to_owned();
        Ok(self.sched.submit(
            &key,
            TaskKind::Copy,
            Priority::Normal,
            Box::new(move |ctx| {
                Box::pin(async move { ops::copy_task(src, dst, from, to, observer, &ctx).await })
            }),
        ))
    }

    /// Move como Task: rename si mismo provider; copy+delete cross-provider.
    ///
    /// # Errors
    /// [`Error::Unsupported`] si algún scheme no tiene provider registrado.
    #[tracing::instrument(skip(self), fields(from = %from.display_lossy(), to = %to.display_lossy()))]
    pub fn move_(&self, from: &VPath, to: &VPath) -> Result<TaskHandle, Error> {
        let src = self.provider_for(from)?;
        let dst = self.provider_for(to)?;
        let observer = Arc::clone(&self.observer);
        let (from, to) = (from.clone(), to.clone());
        let key = to.scheme().to_owned();
        Ok(self.sched.submit(
            &key,
            TaskKind::Move,
            Priority::Normal,
            Box::new(move |ctx| {
                Box::pin(async move { ops::move_task(src, dst, from, to, observer, &ctx).await })
            }),
        ))
    }

    /// Borrado (recursivo post-order) como Task.
    ///
    /// # Errors
    /// [`Error::Unsupported`] si el scheme no tiene provider registrado.
    #[tracing::instrument(skip(self), fields(path = %path.display_lossy()))]
    pub fn delete(&self, path: &VPath) -> Result<TaskHandle, Error> {
        let provider = self.provider_for(path)?;
        let observer = Arc::clone(&self.observer);
        let path = path.clone();
        let key = path.scheme().to_owned();
        Ok(self.sched.submit(
            &key,
            TaskKind::Delete,
            Priority::Normal,
            Box::new(move |ctx| {
                Box::pin(async move { ops::delete_task(provider, path, observer, &ctx).await })
            }),
        ))
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}
