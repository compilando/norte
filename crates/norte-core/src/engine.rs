//! [`Engine`]: la API embebida del core (M0). El daemon JSON-RPC (M1)
//! envolverá esta misma API; los frontends no contienen lógica de negocio.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use norte_proto::{CollisionPolicy, DeleteMode, Entry, Error, SymlinkPolicy, TaskKind, VPath};
use norte_vfs::{EntryStream, Provider};

use crate::observer::{MutationObserver, NoopObserver};
use crate::ops;
use crate::scheduler::{Priority, Scheduler, TaskHandle};

/// Opciones de una copia/movimiento (ADR 0005): qué hacer ante colisiones
/// y con los symlinks. `Default` = el comportamiento estricto de M0
/// (`Fail` + `Preserve`).
///
/// ```
/// use norte_core::TransferOptions;
/// use norte_proto::{CollisionPolicy, SymlinkPolicy};
/// let opts = TransferOptions::default();
/// assert_eq!(opts.on_collision, CollisionPolicy::Fail);
/// assert_eq!(opts.symlinks, SymlinkPolicy::Preserve);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransferOptions {
    /// Qué hacer si el destino ya existe.
    pub on_collision: CollisionPolicy,
    /// Qué hacer con los symlinks del origen.
    pub symlinks: SymlinkPolicy,
}

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

    /// Lectura de un archivo como stream (directa, sin Task), con rango
    /// opcional — el viewer lee cabeceras de archivos enormes sin tragarse
    /// el resto (ADR 0005).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si no hay provider para el scheme; los del provider.
    pub async fn read(
        &self,
        p: &VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> Result<norte_vfs::ByteStream, Error> {
        self.provider_for(p)?.read(p, range).await
    }

    /// Copia (recursiva si es dir) como Task, con las políticas por defecto
    /// (`Fail` + `Preserve`).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si algún scheme no tiene provider registrado.
    pub fn copy(&self, from: &VPath, to: &VPath) -> Result<TaskHandle, Error> {
        self.copy_with(from, to, TransferOptions::default())
    }

    /// Copia con políticas explícitas de colisión y symlinks (ADR 0005).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si algún scheme no tiene provider registrado.
    #[tracing::instrument(skip(self), fields(from = %from.display_lossy(), to = %to.display_lossy()))]
    pub fn copy_with(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
    ) -> Result<TaskHandle, Error> {
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
                Box::pin(
                    async move { ops::copy_task(src, dst, from, to, opts, observer, &ctx).await },
                )
            }),
        ))
    }

    /// Move como Task con las políticas por defecto: rename si mismo
    /// provider; copy+delete con plan único si es cross-provider o el
    /// rename devuelve `Unsupported` (EXDEV).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si algún scheme no tiene provider registrado.
    pub fn move_(&self, from: &VPath, to: &VPath) -> Result<TaskHandle, Error> {
        self.move_with(from, to, TransferOptions::default())
    }

    /// Move con políticas explícitas de colisión y symlinks (ADR 0005).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si algún scheme no tiene provider registrado.
    #[tracing::instrument(skip(self), fields(from = %from.display_lossy(), to = %to.display_lossy()))]
    pub fn move_with(
        &self,
        from: &VPath,
        to: &VPath,
        opts: TransferOptions,
    ) -> Result<TaskHandle, Error> {
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
                Box::pin(
                    async move { ops::move_task(src, dst, from, to, opts, observer, &ctx).await },
                )
            }),
        ))
    }

    /// Borrado PERMANENTE (recursivo post-order) como Task. La papelera
    /// es [`Self::delete_with`] con [`DeleteMode::Trash`].
    ///
    /// # Errors
    /// [`Error::Unsupported`] si el scheme no tiene provider registrado.
    pub fn delete(&self, path: &VPath) -> Result<TaskHandle, Error> {
        self.delete_with(path, DeleteMode::Permanent)
    }

    /// Borrado con modo explícito (ADR 0009): `Trash` mueve el árbol
    /// entero a la papelera del provider (una sola operación; sin la
    /// capability `TRASH` la task falla `Unsupported` — el engine JAMÁS
    /// degrada a permanente por su cuenta); `Permanent` borra de verdad.
    ///
    /// # Errors
    /// [`Error::Unsupported`] si el scheme no tiene provider registrado.
    #[tracing::instrument(skip(self), fields(path = %path.display_lossy(), ?mode))]
    pub fn delete_with(&self, path: &VPath, mode: DeleteMode) -> Result<TaskHandle, Error> {
        let provider = self.provider_for(path)?;
        let observer = Arc::clone(&self.observer);
        let path = path.clone();
        let key = path.scheme().to_owned();
        Ok(self.sched.submit(
            &key,
            TaskKind::Delete,
            Priority::Normal,
            Box::new(move |ctx| {
                Box::pin(
                    async move { ops::delete_task(provider, path, mode, observer, &ctx).await },
                )
            }),
        ))
    }

    /// Capabilities del provider que sirve `p` (para que el frontend
    /// decida, p. ej., si el F8 va a papelera o avisa de permanente).
    ///
    /// # Errors
    /// [`Error::Unsupported`] si el scheme no tiene provider registrado.
    pub fn capabilities(&self, p: &VPath) -> Result<norte_proto::Capabilities, Error> {
        Ok(self.provider_for(p)?.capabilities())
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}
