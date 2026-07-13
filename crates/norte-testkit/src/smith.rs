//! Forjas deterministas de archivos comprimidos hostiles (fase 8, ADR 0018).
//!
//! Las fixtures de zip/tar son CÓDIGO, no binarios commiteados: control
//! byte a byte (nombres crudos cp437, bit 11 mentiroso, zip-slip, EOCD
//! falso) sin `.gitattributes` ni regeneradores. Solo formato `stored`
//! (sin compresión): lo que se testea aquí es estructura y nombres; la
//! descompresión real la cubren los tests del provider con el crate `zip`.

/// CRC-32 (IEEE, reflejado) bit a bit — suficiente para fixtures.
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = !0;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Fecha/hora DOS fija (2020-01-01 00:00:00): determinismo total.
const DOS_TIME: u16 = 0;
const DOS_DATE: u16 = ((2020 - 1980) << 9) | (1 << 5) | 1;

enum ZipEntry {
    /// `utf8_flag` = bit 11 del general purpose flag (nombre declara UTF-8).
    File {
        name: Vec<u8>,
        data: Vec<u8>,
        utf8_flag: bool,
    },
    /// Entrada de directorio explícita (nombre con `/` final).
    Dir { name: Vec<u8> },
}

/// Forja de bytes ZIP entrada a entrada. Ver el módulo para el porqué.
///
/// ```
/// let bytes = norte_testkit::ZipSmith::new()
///     .file(b"docs/hola.txt", b"hola")
///     .dir(b"vacio")
///     .build();
/// assert_eq!(&bytes[..4], b"PK\x03\x04");
/// ```
#[derive(Default)]
pub struct ZipSmith {
    entries: Vec<ZipEntry>,
}

impl ZipSmith {
    /// Forja vacía.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Archivo `stored` con el bit 11 APAGADO (nombre en bytes crudos:
    /// cp437, Latin-1, lo que sea — el caso histórico).
    #[must_use]
    pub fn file(mut self, name: &[u8], data: &[u8]) -> Self {
        self.entries.push(ZipEntry::File {
            name: name.to_vec(),
            data: data.to_vec(),
            utf8_flag: false,
        });
        self
    }

    /// Archivo `stored` con el bit 11 ENCENDIDO (el nombre DECLARA UTF-8 —
    /// nadie verifica que sea verdad: forja también flags mentirosos).
    #[must_use]
    pub fn file_utf8(mut self, name: &[u8], data: &[u8]) -> Self {
        self.entries.push(ZipEntry::File {
            name: name.to_vec(),
            data: data.to_vec(),
            utf8_flag: true,
        });
        self
    }

    /// Entrada de directorio explícita; añade el `/` final si falta.
    #[must_use]
    pub fn dir(mut self, name: &[u8]) -> Self {
        let mut name = name.to_vec();
        if name.last() != Some(&b'/') {
            name.push(b'/');
        }
        self.entries.push(ZipEntry::Dir { name });
        self
    }

    /// Los bytes del ZIP completo (local headers + central directory + EOCD).
    #[must_use]
    pub fn build(self) -> Vec<u8> {
        self.build_with_count(None)
    }

    /// Como [`Self::build`] pero el EOCD MIENTE: declara `claimed` entradas
    /// (bomba de índice barata: anuncia millones sin pagarlos).
    #[must_use]
    pub fn build_lying_eocd(self, claimed: u16) -> Vec<u8> {
        self.build_with_count(Some(claimed))
    }

    #[allow(clippy::cast_possible_truncation)] // fixtures pequeñas por diseño
    fn build_with_count(self, claimed: Option<u16>) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        let real_count = self.entries.len() as u16;
        for entry in &self.entries {
            let (name, data, flags) = match entry {
                ZipEntry::File {
                    name,
                    data,
                    utf8_flag,
                } => (
                    name,
                    data.as_slice(),
                    if *utf8_flag { 1u16 << 11 } else { 0 },
                ),
                ZipEntry::Dir { name } => (name, &[][..], 0),
            };
            let offset = out.len() as u32;
            let crc = crc32(data);
            let size = data.len() as u32;
            // Local file header.
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&flags.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // stored
            out.extend_from_slice(&DOS_TIME.to_le_bytes());
            out.extend_from_slice(&DOS_DATE.to_le_bytes());
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes()); // comprimido
            out.extend_from_slice(&size.to_le_bytes()); // sin comprimir
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra
            out.extend_from_slice(name);
            out.extend_from_slice(data);
            // Central directory entry.
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes()); // made by
            central.extend_from_slice(&20u16.to_le_bytes()); // needed
            central.extend_from_slice(&flags.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes()); // stored
            central.extend_from_slice(&DOS_TIME.to_le_bytes());
            central.extend_from_slice(&DOS_DATE.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&size.to_le_bytes());
            central.extend_from_slice(&size.to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes()); // extra
            central.extend_from_slice(&0u16.to_le_bytes()); // comment
            central.extend_from_slice(&0u16.to_le_bytes()); // disk
            central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            let external: u32 = match entry {
                ZipEntry::Dir { .. } => 0x10, // bit de directorio DOS
                ZipEntry::File { .. } => 0,
            };
            central.extend_from_slice(&external.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name);
        }
        let cd_offset = out.len() as u32;
        let cd_size = central.len() as u32;
        out.extend_from_slice(&central);
        // EOCD.
        let count = claimed.unwrap_or(real_count);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // disco
        out.extend_from_slice(&0u16.to_le_bytes()); // disco del CD
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comentario
        out
    }
}

enum TarEntry {
    File { name: Vec<u8>, data: Vec<u8> },
    Dir { name: Vec<u8> },
    Symlink { name: Vec<u8>, target: Vec<u8> },
}

/// Forja de bytes tar (ustar plano). Nombres > 100 bytes: PANIC — la forja
/// no implementa GNU longname; los nombres largos del corpus se cubren por
/// zip (asimetría documentada, precedente del harness MinIO/255).
///
/// ```
/// let bytes = norte_testkit::TarSmith::new()
///     .file(b"docs/hola.txt", b"hola")
///     .build();
/// assert_eq!(&bytes[257..262], b"ustar");
/// ```
#[derive(Default)]
pub struct TarSmith {
    entries: Vec<TarEntry>,
}

impl TarSmith {
    /// Forja vacía.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Archivo regular.
    #[must_use]
    pub fn file(mut self, name: &[u8], data: &[u8]) -> Self {
        self.entries.push(TarEntry::File {
            name: name.to_vec(),
            data: data.to_vec(),
        });
        self
    }

    /// Directorio explícito; añade el `/` final si falta.
    #[must_use]
    pub fn dir(mut self, name: &[u8]) -> Self {
        let mut name = name.to_vec();
        if name.last() != Some(&b'/') {
            name.push(b'/');
        }
        self.entries.push(TarEntry::Dir { name });
        self
    }

    /// Symlink con target en bytes crudos.
    #[must_use]
    pub fn symlink(mut self, name: &[u8], target: &[u8]) -> Self {
        self.entries.push(TarEntry::Symlink {
            name: name.to_vec(),
            target: target.to_vec(),
        });
        self
    }

    /// Los bytes del tar completo (headers de 512 + datos + 2 bloques cero).
    ///
    /// # Panics
    /// Nombre o target > 100 bytes (ver doc del tipo).
    #[must_use]
    pub fn build(self) -> Vec<u8> {
        let mut out = Vec::new();
        for entry in &self.entries {
            let (name, data, typeflag, link): (&[u8], &[u8], u8, &[u8]) = match entry {
                TarEntry::File { name, data } => (name, data, b'0', &[]),
                TarEntry::Dir { name } => (name, &[], b'5', &[]),
                TarEntry::Symlink { name, target } => (name, &[], b'2', target),
            };
            assert!(
                name.len() <= 100 && link.len() <= 100,
                "TarSmith no forja nombres >100 bytes (usa zip para el corpus largo)"
            );
            let mut header = [0u8; 512];
            header[..name.len()].copy_from_slice(name);
            header[100..107].copy_from_slice(b"0000644"); // mode
            header[108..115].copy_from_slice(b"0000000"); // uid
            header[116..123].copy_from_slice(b"0000000"); // gid
            let size_field = format!("{:011o}", data.len());
            header[124..135].copy_from_slice(size_field.as_bytes());
            header[136..147].copy_from_slice(b"00000000000"); // mtime 1970
            header[148..156].copy_from_slice(b"        "); // chksum en blanco
            header[156] = typeflag;
            header[157..157 + link.len()].copy_from_slice(link);
            header[257..262].copy_from_slice(b"ustar");
            header[263..265].copy_from_slice(b"00");
            let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
            let chk = format!("{sum:06o}\0 ");
            header[148..156].copy_from_slice(chk.as_bytes());
            out.extend_from_slice(&header);
            out.extend_from_slice(data);
            let resto = data.len() % 512;
            if resto != 0 {
                out.extend(std::iter::repeat_n(0u8, 512 - resto));
            }
        }
        out.extend(std::iter::repeat_n(0u8, 1024));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_vectores_conocidos() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn zip_estructura_coherente() {
        let z = ZipSmith::new()
            .file(b"a.txt", b"hola")
            .file_utf8("ñ.txt".as_bytes(), "eñe".as_bytes())
            .dir(b"sub")
            .build();
        assert_eq!(&z[..4], b"PK\x03\x04");
        // EOCD al final, cuenta real = 3.
        let eocd = &z[z.len() - 22..];
        assert_eq!(&eocd[..4], b"PK\x05\x06");
        assert_eq!(u16::from_le_bytes([eocd[10], eocd[11]]), 3);
        // El offset del CD apunta a una firma de central directory.
        let cd_off = u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]) as usize;
        assert_eq!(&z[cd_off..cd_off + 4], b"PK\x01\x02");
    }

    #[test]
    fn zip_eocd_mentiroso() {
        let z = ZipSmith::new().file(b"x", b"").build_lying_eocd(60_000);
        let eocd = &z[z.len() - 22..];
        assert_eq!(u16::from_le_bytes([eocd[10], eocd[11]]), 60_000);
    }

    #[test]
    fn tar_estructura_coherente() {
        let t = TarSmith::new()
            .file(b"docs/x.bin", &[0xFF; 700])
            .symlink(b"lnk", b"docs/x.bin")
            .build();
        // header + 700 pad a 1024 + header symlink + 1024 de cierre.
        assert_eq!(t.len(), 512 + 1024 + 512 + 1024);
        assert_eq!(&t[257..262], b"ustar");
        // checksum: recalcular con el campo en blanco coincide.
        let mut h = t[..512].to_vec();
        let stored: Vec<u8> = h[148..156].to_vec();
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        assert_eq!(format!("{sum:06o}\0 ").as_bytes(), stored.as_slice());
    }

    #[test]
    #[should_panic(expected = "no forja nombres >100")]
    fn tar_nombre_largo_panica() {
        let _ = TarSmith::new().file(&[b'a'; 101], b"").build();
    }
}
