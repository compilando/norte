//! Writing an archive: `zip`, `tar` and `tar.gz`, entry by entry and
//! without touching disk (#132).
//!
//! **This isn't the provider.** `norte-vfs-archive` is still `READ_ONLY`
//! (ADR 0018) and nothing here mutates a container's inside: what's here
//! is a pure encoder — metadata and bytes in, archive bytes out — that the
//! core uses to MANUFACTURE a new file through the destination's
//! provider, whatever it is. That's why it lives in this crate and knows
//! no `Provider`.
//!
//! The contract is incremental for the same reason: an entry's bytes
//! arrive in pieces from an async provider, and the archive is sent to a
//! destination that may be remote. Nothing requires keeping either the
//! archive or an entry in memory.
//!
//! ```
//! use norte_vfs_archive::write::{ArchiveWriter, PackEntry, PackFormat};
//!
//! let mut w = ArchiveWriter::new(PackFormat::Zip, 6);
//! w.begin(&PackEntry::file(b"hi.txt".to_vec(), 2)).unwrap();
//! w.data(b"hi").unwrap();
//! w.end().unwrap();
//! w.finish().unwrap();
//! let bytes = w.take();
//! assert_eq!(&bytes[..2], b"PK");
//! ```

mod tar;
mod zip;

pub use tar::TarWriter;
pub use zip::ZipWriter;

/// Formats that can be WRITTEN.
///
/// Fewer than the ones that can be read, and on purpose: `rar` is
/// delegated to an external program in read mode (ADR 0056) and 7z isn't
/// even read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackFormat {
    /// zip with `deflate` (or `store` at level 0).
    Zip,
    /// plain tar.
    Tar,
    /// tar compressed with gzip.
    TarGz,
}

impl PackFormat {
    /// The format's token exactly as it travels on the wire and as
    /// [`ARCHIVE_FORMATS`](norte_proto::ARCHIVE_FORMATS) names it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::Tar => "tar",
            Self::TarGz => "tar+gz",
        }
    }

    /// The format a file NAME suggests, or `None` if none does.
    ///
    /// It's sugar for the frontend, which fills in the dialog: what
    /// really decides is the wire field, because guessing a name's format
    /// on the server would be deciding for the user without telling them.
    #[must_use]
    pub fn from_name(name: &[u8]) -> Option<Self> {
        let ends_with = |suf: &[u8]| {
            name.len() >= suf.len() && name[name.len() - suf.len()..].eq_ignore_ascii_case(suf)
        };
        if ends_with(b".tar.gz") || ends_with(b".tgz") {
            return Some(Self::TarGz);
        }
        if ends_with(b".tar") {
            return Some(Self::Tar);
        }
        if ends_with(b".zip") {
            return Some(Self::Zip);
        }
        None
    }
}

/// Why an archive couldn't be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PackError {
    /// The name doesn't fit the format (zip carries it in 16 bits).
    #[error("entry name does not fit the format")]
    Name,
    /// Called out of order: data with no entry open, two entries at once,
    /// closing twice.
    #[error("archive writer used out of order")]
    State,
    /// The delivered bytes don't match the announced size (tar carries it
    /// in the header, BEFORE the data).
    #[error("entry size does not match the bytes written")]
    Size,
    /// The compressor failed.
    #[error("compressor failed")]
    Io,
}

/// What's known about an entry BEFORE writing its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackEntry {
    /// The name INSIDE the archive, relative and in raw bytes (rule 1).
    /// Whoever packs composes it from the base; it isn't interpreted here.
    pub name: Vec<u8>,
    /// Exact size. Tar needs it upfront; zip checks it.
    pub size: u64,
    /// `true` for a directory (no data, with the trailing slash zip requires).
    pub dir: bool,
    /// Permissions, if the source gave them.
    pub mode: Option<u32>,
    /// Modification time in epoch milliseconds, if the source gave it.
    pub mtime_ms: Option<i64>,
}

impl PackEntry {
    /// A file entry with the bare minimum.
    #[must_use]
    pub fn file(name: Vec<u8>, size: u64) -> Self {
        Self {
            name,
            size,
            dir: false,
            mode: None,
            mtime_ms: None,
        }
    }

    /// A directory entry.
    #[must_use]
    pub fn dir(name: Vec<u8>) -> Self {
        Self {
            name,
            size: 0,
            dir: true,
            mode: None,
            mtime_ms: None,
        }
    }
}

/// The writer, whatever the format.
///
/// Cycle: `begin` → `data`* → `end` per entry, `finish` when done, and
/// [`ArchiveWriter::take`] whenever you want to drain what's been produced.
pub enum ArchiveWriter {
    /// zip.
    Zip(Box<ZipWriter>),
    /// plain tar.
    Tar(Box<TarWriter>),
    /// tar inside gzip: the tar is produced the same way and piped
    /// through the compressor, which is exactly what `tar.gz` is.
    TarGz {
        /// The tar inside.
        inner: Box<TarWriter>,
        /// The compressor, drained on every `take`.
        gz: Box<flate2::write::GzEncoder<Vec<u8>>>,
    },
}

impl ArchiveWriter {
    /// A writer for the given format. `level` is 0..=9 and only the
    /// compressed formats look at it.
    #[must_use]
    pub fn new(format: PackFormat, level: u32) -> Self {
        let level = level.min(9);
        match format {
            PackFormat::Zip => Self::Zip(Box::new(ZipWriter::new(level))),
            PackFormat::Tar => Self::Tar(Box::default()),
            PackFormat::TarGz => Self::TarGz {
                inner: Box::default(),
                gz: Box::new(flate2::write::GzEncoder::new(
                    Vec::new(),
                    flate2::Compression::new(level),
                )),
            },
        }
    }

    /// Opens an entry.
    ///
    /// # Errors
    ///
    /// [`PackError`]'s.
    pub fn begin(&mut self, entry: &PackEntry) -> Result<(), PackError> {
        match self {
            Self::Zip(w) => w.begin(entry),
            Self::Tar(w) | Self::TarGz { inner: w, .. } => w.begin(entry),
        }
    }

    /// Adds data to the open entry.
    ///
    /// # Errors
    ///
    /// [`PackError`]'s.
    pub fn data(&mut self, chunk: &[u8]) -> Result<(), PackError> {
        match self {
            Self::Zip(w) => w.data(chunk),
            Self::Tar(w) | Self::TarGz { inner: w, .. } => w.data(chunk),
        }
    }

    /// Closes the open entry.
    ///
    /// # Errors
    ///
    /// [`PackError`]'s.
    pub fn end(&mut self) -> Result<(), PackError> {
        match self {
            Self::Zip(w) => w.end(),
            Self::Tar(w) | Self::TarGz { inner: w, .. } => w.end(),
        }
    }

    /// Closes the archive.
    ///
    /// # Errors
    ///
    /// [`PackError`]'s.
    pub fn finish(&mut self) -> Result<(), PackError> {
        match self {
            Self::Zip(w) => w.finish(),
            Self::Tar(w) => w.finish(),
            Self::TarGz { inner, gz } => {
                inner.finish()?;
                let tail = inner.take();
                std::io::Write::write_all(gz.as_mut(), &tail).map_err(|_| PackError::Io)?;
                gz.try_finish().map_err(|_| PackError::Io)
            }
        }
    }

    /// Drains what's been produced so far.
    ///
    /// # Panics
    ///
    /// Never: `write_all` over an in-memory `Vec` doesn't fail, and if the
    /// compressor had failed, `data` would already have said so.
    pub fn take(&mut self) -> Vec<u8> {
        match self {
            Self::Zip(w) => w.take(),
            Self::Tar(w) => w.take(),
            Self::TarGz { inner, gz } => {
                let raw = inner.take();
                if !raw.is_empty() {
                    // The tar inside is handed to the compressor as it
                    // comes out, and whatever the compressor has produced
                    // is delivered. That way neither the tar nor the gz
                    // accumulates the whole archive.
                    let _ = std::io::Write::write_all(gz.as_mut(), &raw);
                }
                std::mem::take(gz.get_mut())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The user decides the name, so the dialog's default format comes
    /// from it — and `.tar.gz` wins over `.tar`, which is its suffix.
    #[test]
    fn the_format_is_suggested_by_the_name() {
        assert_eq!(PackFormat::from_name(b"a.zip"), Some(PackFormat::Zip));
        assert_eq!(PackFormat::from_name(b"A.ZIP"), Some(PackFormat::Zip));
        assert_eq!(PackFormat::from_name(b"a.tar"), Some(PackFormat::Tar));
        assert_eq!(PackFormat::from_name(b"a.tar.gz"), Some(PackFormat::TarGz));
        assert_eq!(PackFormat::from_name(b"a.tgz"), Some(PackFormat::TarGz));
        assert_eq!(PackFormat::from_name(b"a.rar"), None, "rar isn't written");
        assert_eq!(PackFormat::from_name(b"none"), None);
    }

    /// Using the writer out of order is an error, not a weird archive.
    #[test]
    fn the_order_of_calls_is_checked() {
        for f in [PackFormat::Zip, PackFormat::Tar, PackFormat::TarGz] {
            let mut w = ArchiveWriter::new(f, 6);
            assert_eq!(w.data(b"x"), Err(PackError::State), "{f:?}");
            assert_eq!(w.end(), Err(PackError::State), "{f:?}");
            w.begin(&PackEntry::file(b"a".to_vec(), 1)).expect("opens");
            assert_eq!(
                w.begin(&PackEntry::file(b"b".to_vec(), 1)),
                Err(PackError::State),
                "{f:?}: two entries at once, no"
            );
        }
    }

    /// Tar carries the size in the header, BEFORE the data: delivering
    /// something else is a tar nobody can read past that entry, so it's
    /// said instead of written.
    #[test]
    fn tar_does_not_let_the_size_lie() {
        let mut w = ArchiveWriter::new(PackFormat::Tar, 0);
        w.begin(&PackEntry::file(b"a".to_vec(), 4)).expect("opens");
        assert_eq!(w.data(b"12345"), Err(PackError::Size), "too much");
        w.data(b"123").expect("too little goes in");
        assert_eq!(w.end(), Err(PackError::Size), "and it shows at close");
    }
}
