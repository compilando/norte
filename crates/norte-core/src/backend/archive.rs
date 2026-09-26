//! [`Backend`](super::Backend)'s packed-archive area (#132):
//! `archive.pack`/`archive.test` with their reports, and `file.split`/`file.combine`.

use norte_proto::Error;

use super::{Backend, TaskRef};

impl Backend {
    /// Builds an archive (`archive.pack`, 0.50.0, #132).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPath`] with no sources, and whatever the core
    /// returns. An N-1 daemon with no such method answers `METHOD_NOT_FOUND`
    /// → [`Error::Unsupported`].
    pub async fn pack(
        &self,
        params: norte_proto::methods::ArchivePackParams,
    ) -> Result<TaskRef, Error> {
        if params.sources.is_empty() {
            return Err(Error::InvalidPath);
        }
        match self {
            Self::Embedded(engine) => {
                // The report (#250) is collected via `archive_pack_report`:
                // only the handle travels here.
                let handle = engine.pack_as(params, crate::journal::Actor::User).await?;
                Ok(TaskRef::from_handle(&handle))
            }
            Self::Remote(r) => r.pack(params).await.map(TaskRef::from),
        }
    }

    /// Tests an archive (`archive.test`, 0.50.0, #132).
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] if the name isn't a known format, and whatever
    /// the core returns.
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
            Self::Remote(r) => r.test_archive(params).await.map(TaskRef::from),
        }
    }

    /// The report for an `archive.test` already launched (0.50.0, #132).
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] if that id was never a test of this instance, if
    /// the ring already evicted it, or if it belongs to another actor —
    /// all three get the same answer, which is what the daemon does.
    pub async fn archive_test_report(
        &self,
        task_id: norte_proto::TaskId,
    ) -> Result<norte_proto::methods::ArchiveTestResult, Error> {
        match self {
            Self::Embedded(engine) => engine
                .archive_test_report(task_id)
                .map(|(_, r)| r)
                .ok_or(Error::NotFound),
            Self::Remote(r) => r.archive_test_report(task_id).await,
        }
    }

    /// The report for an `archive.pack` (0.58.0, #250): what that packing
    /// job stored that doesn't survive leaving here.
    ///
    /// # Errors
    /// [`Error::NotFound`] if that id was never a packing job or if the ring
    /// already evicted it; against an N-1 daemon, whatever it answers.
    pub async fn archive_pack_report(
        &self,
        task_id: norte_proto::TaskId,
    ) -> Result<norte_proto::methods::ArchivePackReportResult, Error> {
        match self {
            Self::Embedded(engine) => engine
                .archive_pack_report(task_id)
                .map(|(_, r)| r)
                .ok_or(Error::NotFound),
            Self::Remote(r) => r.archive_pack_report(task_id).await,
        }
    }

    /// Splits a file into pieces (`file.split`, 0.50.0, #132).
    ///
    /// # Errors
    ///
    /// Whatever the core returns: a piece too small, too many pieces, or an
    /// I/O failure.
    pub async fn split_file(
        &self,
        params: norte_proto::methods::FileSplitParams,
    ) -> Result<TaskRef, Error> {
        match self {
            Self::Embedded(engine) => {
                let handle = engine.split_as(params, crate::journal::Actor::User).await?;
                Ok(TaskRef::from_handle(&handle))
            }
            Self::Remote(r) => r.split_file(params).await.map(TaskRef::from),
        }
    }

    /// Joins the pieces of a split (`file.combine`, 0.50.0, #132).
    ///
    /// # Errors
    ///
    /// Whatever the core returns: a gap in the numbering, a short
    /// intermediate piece, or an I/O failure.
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
            Self::Remote(r) => r.combine_files(params).await.map(TaskRef::from),
        }
    }
}
