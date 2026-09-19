//! El área de archivos empaquetados de [`Backend`](super::Backend) (#132):
//! `archive.pack`/`archive.test` con sus informes, y `file.split`/`file.combine`.

use norte_proto::Error;

use super::{Backend, TaskRef};

impl Backend {
    /// Fabrica un archivo (`archive.pack`, 0.50.0, #132).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPath`] sin fuentes, y lo que devuelva el core. Un
    /// daemon N-1 sin el método contesta `METHOD_NOT_FOUND` →
    /// [`Error::Unsupported`].
    pub async fn pack(
        &self,
        params: norte_proto::methods::ArchivePackParams,
    ) -> Result<TaskRef, Error> {
        if params.sources.is_empty() {
            return Err(Error::InvalidPath);
        }
        match self {
            Self::Embedded(engine) => {
                // El informe (#250) se recoge por `archive_pack_report`: aquí
                // solo viaja el handle.
                let handle = engine.pack_as(params, crate::journal::Actor::User).await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.pack(params).await.map(TaskRef::from),
        }
    }

    /// Comprueba un archivo (`archive.test`, 0.50.0, #132).
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] si el nombre no es de un formato conocido, y lo
    /// que devuelva el core.
    pub async fn test_archive(
        &self,
        params: norte_proto::methods::ArchiveTestParams,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let (handle, _) = engine
                    .test_archive_as(params, crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.test_archive(params).await.map(TaskRef::from),
        }
    }

    /// El informe de un `archive.test` ya lanzado (0.50.0, #132).
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] si ese id no fue un test de esta instancia, si el
    /// anillo ya lo desalojó o si es de otro actor — las tres con la misma
    /// respuesta, que es lo que hace el daemon.
    pub async fn archive_test_report(
        &self,
        task_id: norte_proto::TaskId,
    ) -> Result<norte_proto::methods::ArchiveTestResult, Error> {
        match self {
            Self::Embedded(engine) => engine
                .archive_test_report(task_id)
                .map(|(_, r)| r)
                .ok_or(Error::NotFound),
            #[cfg(unix)]
            Self::Remote(r) => r.archive_test_report(task_id).await,
        }
    }

    /// El informe de un `archive.pack` (0.58.0, #250): qué guardó ese
    /// empaquetado que no sobrevive a salir de aquí.
    ///
    /// # Errors
    /// [`Error::NotFound`] si ese id nunca fue un empaquetado o si el anillo ya
    /// lo desalojó; contra un daemon N-1, lo que responda él.
    pub async fn archive_pack_report(
        &self,
        task_id: norte_proto::TaskId,
    ) -> Result<norte_proto::methods::ArchivePackReportResult, Error> {
        match self {
            Self::Embedded(engine) => engine
                .archive_pack_report(task_id)
                .map(|(_, r)| r)
                .ok_or(Error::NotFound),
            #[cfg(unix)]
            Self::Remote(r) => r.archive_pack_report(task_id).await,
        }
    }

    /// Parte un fichero en trozos (`file.split`, 0.50.0, #132).
    ///
    /// # Errors
    ///
    /// Lo que devuelva el core: trozo demasiado pequeño, demasiados trozos, o
    /// un fallo de I/O.
    pub async fn split_file(
        &self,
        params: norte_proto::methods::FileSplitParams,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let handle = engine.split_as(params, crate::journal::Actor::User).await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.split_file(params).await.map(TaskRef::from),
        }
    }

    /// Junta los trozos de un split (`file.combine`, 0.50.0, #132).
    ///
    /// # Errors
    ///
    /// Lo que devuelva el core: un hueco en la numeración, un trozo intermedio
    /// corto, o un fallo de I/O.
    pub async fn combine_files(
        &self,
        params: norte_proto::methods::FileCombineParams,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let handle = engine
                    .combine_as(params, crate::journal::Actor::User)
                    .await?;
                Ok(TaskRef::from_handle(&handle))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.combine_files(params).await.map(TaskRef::from),
        }
    }
}
