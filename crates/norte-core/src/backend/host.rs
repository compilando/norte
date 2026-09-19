//! El área de host de [`Backend`](super::Backend): GC de staging huérfano,
//! volúmenes del host, y el registro del daemon (`log.tail`/`log.level`).

use norte_proto::{Error, VPath};

use super::{Backend, volume_to_proto};

impl Backend {
    /// GC de staging `.norte-partial` huérfano bajo `dir` (#11, ADR 0012):
    /// operación PUNTUAL, no una Task ni una mutación del journal. Devuelve
    /// cuántos barrió.
    ///
    /// # Errors
    /// En `Remote` es [`Error::Unsupported`]: no existe (aún) un método de
    /// wire para el GC — exponerlo exige un cambio de protocolo, diferido
    /// hasta que haya demanda. En `Embedded`, los del provider.
    pub async fn gc_partials(
        &self,
        dir: &VPath,
        older_than: std::time::Duration,
    ) -> Result<usize, Error> {
        match self {
            Self::Embedded(engine) => engine.gc_partials(dir, older_than).await,
            #[cfg(unix)]
            Self::Remote(_) => Err(Error::Unsupported),
        }
    }

    /// Volúmenes del host (`host.volumes`, 0.37.0, #131): mount point, tipo
    /// de filesystem, kind y espacio libre/total. `include_pseudo` es el
    /// toggle "mostrar todo" del picker (diseño §E de
    /// `2026-08-10-volumes-design.md`).
    ///
    /// Embebido: llama a [`crate::volumes::enumerate`] directamente — un
    /// volumen es del HOST, no de un provider, así que no hay engine que
    /// consultar (diseño §A). SIN gate de actor: un core embebido no tiene
    /// conexión ni daemon, así que quien lo llama YA es el humano sentado
    /// delante — no hay superficie remota que sandboxear.
    ///
    /// Remoto: `host.volumes` contra el daemon, que SÍ gatea por actor de
    /// conexión (diseño §C) — una conexión de agente ve
    /// [`Error::PolicyDenied`].
    ///
    /// # Errors
    /// Taxonomía del protocolo; con el daemon caído,
    /// `ProviderUnavailable{retryable:true}`.
    pub async fn volumes(
        &self,
        include_pseudo: bool,
    ) -> Result<Vec<norte_proto::methods::Volume>, Error> {
        match self {
            Self::Embedded(_) => {
                let volumes = crate::volumes::enumerate(include_pseudo)
                    .await
                    .map_err(|_| Error::Io { retryable: false })?;
                Ok(volumes.into_iter().map(volume_to_proto).collect())
            }
            #[cfg(unix)]
            Self::Remote(r) => r.volumes(include_pseudo).await,
        }
    }

    /// El registro del DAEMON desde `cursor` (`log.tail`, 0.65.0, #328,
    /// ADR 0092).
    ///
    /// `cursor: None` es «dame lo que haya» y NO es lo mismo que cero: contra
    /// un anillo que ya dio la vuelta, un cero reportaría un `lost` falso en
    /// la primera vuelta. Después se encadena el `next` que llegó.
    ///
    /// # Errors
    /// Taxonomía del protocolo. En `Embedded` es siempre
    /// [`Error::Unsupported`], y eso NO es una carencia: el anillo del core
    /// embebido está en ESTE proceso, así que ya es el que el frontend lee —
    /// no hay una segunda fuente que ofrecer. En `Remote`, un daemon de la
    /// misma versión compilado sin la feature `logging` contesta lo mismo, y
    /// esa respuesta no puede cambiar mientras ese daemon viva.
    ///
    /// Las dos respuestas se escriben igual y **no significan lo mismo**, así
    /// que quien pinta un panel decide con [`Self::is_remote`] antes de
    /// preguntar: sin daemon no hay nada de lo que informar, y una frase sobre
    /// «este daemon» donde no hay ninguno es peor que el silencio.
    ///
    /// ```
    /// use norte_core::{Engine, backend::Backend};
    /// use norte_proto::Error;
    /// use std::sync::Arc;
    /// let rt = tokio::runtime::Runtime::new().expect("runtime");
    /// let backend = Backend::Embedded(Arc::new(Engine::new()));
    /// assert!(matches!(
    ///     rt.block_on(backend.log_tail(None, 10)),
    ///     Err(Error::Unsupported)
    /// ));
    /// ```
    pub async fn log_tail(
        &self,
        cursor: Option<u64>,
        max: u32,
    ) -> Result<norte_proto::methods::LogTailResult, Error> {
        match self {
            Self::Embedded(_) => Err(Error::Unsupported),
            #[cfg(unix)]
            Self::Remote(r) => r.log_tail(cursor, max).await,
        }
    }

    /// Sube el nivel que el anillo del daemon captura (`log.level`, 0.65.0,
    /// #328) y devuelve el que de verdad quedó puesto.
    ///
    /// Su anillo es SUYO: es global a todos sus clientes y nunca baja, así
    /// que lo pedido y lo puesto no tienen por qué coincidir — de ahí que
    /// esto devuelva un nivel en vez de un `()`.
    ///
    /// # Errors
    /// Taxonomía del protocolo; [`Error::Unsupported`] en `Embedded` y contra
    /// un daemon sin registro que servir (ver [`Self::log_tail`]).
    ///
    /// ```
    /// use norte_core::{Engine, backend::Backend};
    /// use norte_proto::Error;
    /// use std::sync::Arc;
    /// let rt = tokio::runtime::Runtime::new().expect("runtime");
    /// let backend = Backend::Embedded(Arc::new(Engine::new()));
    /// assert!(matches!(
    ///     rt.block_on(backend.log_level("debug")),
    ///     Err(Error::Unsupported)
    /// ));
    /// ```
    pub async fn log_level(&self, level: &str) -> Result<String, Error> {
        match self {
            Self::Embedded(_) => Err(Error::Unsupported),
            #[cfg(unix)]
            Self::Remote(r) => r.log_level(level).await,
        }
    }
}
