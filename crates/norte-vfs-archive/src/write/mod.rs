//! Escribir un archivo: `zip`, `tar` y `tar.gz`, entrada a entrada y sin
//! tocar el disco (#132).
//!
//! **Esto no es el provider.** `norte-vfs-archive` sigue siendo `READ_ONLY`
//! (ADR 0018) y nada de aquí muta el interior de un contenedor: lo que hay es
//! un codificador puro —metadatos y bytes en, bytes de archivo fuera— que el
//! core usa para FABRICAR un fichero nuevo a través del provider del destino,
//! sea cual sea. Por eso vive en este crate y no conoce ningún `Provider`.
//!
//! El contrato es incremental por la misma razón: los bytes de una entrada
//! llegan a trozos desde un provider asíncrono, y el archivo se manda a un
//! destino que puede ser remoto. Nada obliga a tener en memoria ni el archivo
//! ni una entrada.
//!
//! ```
//! use norte_vfs_archive::write::{ArchiveWriter, PackEntry, PackFormat};
//!
//! let mut w = ArchiveWriter::new(PackFormat::Zip, 6);
//! w.begin(&PackEntry::file(b"hola.txt".to_vec(), 4)).unwrap();
//! w.data(b"hola").unwrap();
//! w.end().unwrap();
//! w.finish().unwrap();
//! let bytes = w.take();
//! assert_eq!(&bytes[..2], b"PK");
//! ```

mod tar;
mod zip;

pub use tar::TarWriter;
pub use zip::ZipWriter;

/// Formatos que se saben ESCRIBIR.
///
/// Menos de los que se saben leer, y a propósito: `rar` se delega a un
/// programa externo en modo lectura (ADR 0056) y 7z no se lee siquiera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackFormat {
    /// zip con `deflate` (o `store` a nivel 0).
    Zip,
    /// tar plano.
    Tar,
    /// tar comprimido con gzip.
    TarGz,
}

impl PackFormat {
    /// El token del formato tal y como viaja por el wire y como lo nombran
    /// [`ARCHIVE_FORMATS`](norte_proto::ARCHIVE_FORMATS).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::Tar => "tar",
            Self::TarGz => "tar+gz",
        }
    }

    /// El formato que sugiere un NOMBRE de fichero, o `None` si no lo sugiere
    /// ninguno.
    ///
    /// Es azúcar para el frontend, que rellena el diálogo: lo que decide de
    /// verdad es el campo del wire, porque adivinar el formato de un nombre en
    /// el servidor sería decidir por el usuario sin decírselo.
    #[must_use]
    pub fn from_name(name: &[u8]) -> Option<Self> {
        let acaba = |suf: &[u8]| {
            name.len() >= suf.len() && name[name.len() - suf.len()..].eq_ignore_ascii_case(suf)
        };
        if acaba(b".tar.gz") || acaba(b".tgz") {
            return Some(Self::TarGz);
        }
        if acaba(b".tar") {
            return Some(Self::Tar);
        }
        if acaba(b".zip") {
            return Some(Self::Zip);
        }
        None
    }
}

/// Por qué no se pudo empaquetar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PackError {
    /// El nombre no cabe en el formato (zip lo lleva en 16 bits).
    #[error("entry name does not fit the format")]
    Nombre,
    /// Se llamó fuera de orden: datos sin entrada abierta, dos entradas a la
    /// vez, cerrar dos veces.
    #[error("archive writer used out of order")]
    Estado,
    /// Los bytes entregados no cuadran con el tamaño anunciado (tar lo lleva
    /// en la cabecera, ANTES de los datos).
    #[error("entry size does not match the bytes written")]
    Tamano,
    /// El compresor falló.
    #[error("compressor failed")]
    Io,
}

/// Lo que se sabe de una entrada ANTES de escribir sus bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackEntry {
    /// El nombre DENTRO del archivo, relativo y en bytes crudos (regla 1).
    /// Quien empaqueta lo compone a partir de la base; aquí no se interpreta.
    pub name: Vec<u8>,
    /// Tamaño exacto. Tar lo necesita por delante; zip lo comprueba.
    pub size: u64,
    /// `true` para un directorio (sin datos, con la barra final que zip pide).
    pub dir: bool,
    /// Permisos, si el origen los dio.
    pub mode: Option<u32>,
    /// Modificación en milisegundos epoch, si el origen la dio.
    pub mtime_ms: Option<i64>,
}

impl PackEntry {
    /// Una entrada de fichero con lo mínimo.
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

    /// Una entrada de directorio.
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

/// El escritor, sea cual sea el formato.
///
/// Ciclo: `begin` → `data`* → `end` por entrada, `finish` al acabar, y
/// [`ArchiveWriter::take`] cuando se quiera drenar lo producido.
pub enum ArchiveWriter {
    /// zip.
    Zip(Box<ZipWriter>),
    /// tar plano.
    Tar(Box<TarWriter>),
    /// tar dentro de gzip: el tar se produce igual y se pasa por el
    /// compresor, que es exactamente lo que `tar.gz` es.
    TarGz {
        /// El tar de dentro.
        inner: Box<TarWriter>,
        /// El compresor, drenado en cada `take`.
        gz: Box<flate2::write::GzEncoder<Vec<u8>>>,
    },
}

impl ArchiveWriter {
    /// Un escritor del formato dado. `nivel` es 0..=9 y solo lo miran los
    /// formatos comprimidos.
    #[must_use]
    pub fn new(format: PackFormat, nivel: u32) -> Self {
        let nivel = nivel.min(9);
        match format {
            PackFormat::Zip => Self::Zip(Box::new(ZipWriter::new(nivel))),
            PackFormat::Tar => Self::Tar(Box::default()),
            PackFormat::TarGz => Self::TarGz {
                inner: Box::default(),
                gz: Box::new(flate2::write::GzEncoder::new(
                    Vec::new(),
                    flate2::Compression::new(nivel),
                )),
            },
        }
    }

    /// Abre una entrada.
    ///
    /// # Errors
    ///
    /// Las de [`PackError`].
    pub fn begin(&mut self, entry: &PackEntry) -> Result<(), PackError> {
        match self {
            Self::Zip(w) => w.begin(entry),
            Self::Tar(w) | Self::TarGz { inner: w, .. } => w.begin(entry),
        }
    }

    /// Añade datos a la entrada abierta.
    ///
    /// # Errors
    ///
    /// Las de [`PackError`].
    pub fn data(&mut self, chunk: &[u8]) -> Result<(), PackError> {
        match self {
            Self::Zip(w) => w.data(chunk),
            Self::Tar(w) | Self::TarGz { inner: w, .. } => w.data(chunk),
        }
    }

    /// Cierra la entrada abierta.
    ///
    /// # Errors
    ///
    /// Las de [`PackError`].
    pub fn end(&mut self) -> Result<(), PackError> {
        match self {
            Self::Zip(w) => w.end(),
            Self::Tar(w) | Self::TarGz { inner: w, .. } => w.end(),
        }
    }

    /// Cierra el archivo.
    ///
    /// # Errors
    ///
    /// Las de [`PackError`].
    pub fn finish(&mut self) -> Result<(), PackError> {
        match self {
            Self::Zip(w) => w.finish(),
            Self::Tar(w) => w.finish(),
            Self::TarGz { inner, gz } => {
                inner.finish()?;
                let cola = inner.take();
                std::io::Write::write_all(gz.as_mut(), &cola).map_err(|_| PackError::Io)?;
                gz.try_finish().map_err(|_| PackError::Io)
            }
        }
    }

    /// Drena lo producido hasta ahora.
    ///
    /// # Panics
    ///
    /// Nunca: el `write_all` sobre un `Vec` en memoria no falla, y si el
    /// compresor fallara ya lo habría dicho `data`.
    pub fn take(&mut self) -> Vec<u8> {
        match self {
            Self::Zip(w) => w.take(),
            Self::Tar(w) => w.take(),
            Self::TarGz { inner, gz } => {
                let crudo = inner.take();
                if !crudo.is_empty() {
                    // El tar de dentro se le pasa al compresor según sale, y
                    // lo que el compresor lleve producido se entrega. Así ni
                    // el tar ni el gz acumulan el archivo entero.
                    let _ = std::io::Write::write_all(gz.as_mut(), &crudo);
                }
                std::mem::take(gz.get_mut())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El nombre lo decide el usuario, así que el formato por defecto del
    /// diálogo sale de él — y `.tar.gz` gana a `.tar`, que es su sufijo.
    #[test]
    fn el_formato_se_sugiere_por_el_nombre() {
        assert_eq!(PackFormat::from_name(b"a.zip"), Some(PackFormat::Zip));
        assert_eq!(PackFormat::from_name(b"A.ZIP"), Some(PackFormat::Zip));
        assert_eq!(PackFormat::from_name(b"a.tar"), Some(PackFormat::Tar));
        assert_eq!(PackFormat::from_name(b"a.tar.gz"), Some(PackFormat::TarGz));
        assert_eq!(PackFormat::from_name(b"a.tgz"), Some(PackFormat::TarGz));
        assert_eq!(PackFormat::from_name(b"a.rar"), None, "rar no se escribe");
        assert_eq!(PackFormat::from_name(b"sin"), None);
    }

    /// Usar el escritor fuera de orden es un error, no un archivo raro.
    #[test]
    fn el_orden_de_las_llamadas_se_comprueba() {
        for f in [PackFormat::Zip, PackFormat::Tar, PackFormat::TarGz] {
            let mut w = ArchiveWriter::new(f, 6);
            assert_eq!(w.data(b"x"), Err(PackError::Estado), "{f:?}");
            assert_eq!(w.end(), Err(PackError::Estado), "{f:?}");
            w.begin(&PackEntry::file(b"a".to_vec(), 1)).expect("abre");
            assert_eq!(
                w.begin(&PackEntry::file(b"b".to_vec(), 1)),
                Err(PackError::Estado),
                "{f:?}: dos entradas a la vez, no"
            );
        }
    }

    /// Tar lleva el tamaño en la cabecera, DELANTE de los datos: entregar
    /// otra cosa es un tar que nadie puede leer más allá de esa entrada, así
    /// que se dice en vez de escribirlo.
    #[test]
    fn el_tar_no_deja_mentir_al_tamano() {
        let mut w = ArchiveWriter::new(PackFormat::Tar, 0);
        w.begin(&PackEntry::file(b"a".to_vec(), 4)).expect("abre");
        assert_eq!(w.data(b"12345"), Err(PackError::Tamano), "de más");
        w.data(b"123").expect("de menos entra");
        assert_eq!(w.end(), Err(PackError::Tamano), "y se nota al cerrar");
    }
}
