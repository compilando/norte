//! Forjas deterministas de archivos comprimidos hostiles (fase 8, ADR 0018).
//!
//! Las fixtures de zip/tar son CÓDIGO, no binarios commiteados: control
//! byte a byte (nombres crudos cp437, bit 11 mentiroso, zip-slip, EOCD
//! falso) sin `.gitattributes` ni regeneradores. La forja no comprime:
//! `stored` por defecto; una entrada deflate real va por
//! [`ZipSmith::file_deflate`] con los bytes YA comprimidos por el caller
//! (#59 — la descompresión la ejercitan los tests del provider).

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
    /// Entrada con method/flags CRUDOS (cifrado simulado bit 0, métodos
    /// exóticos): `data` va tal cual — las fixtures no se descomprimen.
    Raw {
        name: Vec<u8>,
        data: Vec<u8>,
        method: u16,
        flags: u16,
    },
    /// Entrada `stored` con un extra field CRUDO en el CENTRAL directory
    /// (#59: 0x7075 Info-ZIP, zip64 forjado, basura arbitraria). El local
    /// header queda SIN extra: lo que se testea vive en el CD.
    WithExtra {
        name: Vec<u8>,
        data: Vec<u8>,
        extra: Vec<u8>,
    },
    /// Entrada deflate REAL (#59): `deflated` son los bytes YA comprimidos
    /// (el caller los produce, p. ej. con flate2 — esta forja no comprime);
    /// crc y tamaño sin comprimir se calculan de `uncomp`.
    Deflate {
        name: Vec<u8>,
        uncomp: Vec<u8>,
        deflated: Vec<u8>,
    },
}

/// Qué final lleva el zip forjado: EOCD clásico o cadena zip64 (#59).
enum ZipEnd {
    /// EOCD de 22 bytes; `claimed` fuerza una cuenta mentirosa.
    Classic { claimed: Option<u16> },
    /// EOCD64 + locator + EOCD con marcadores (`0xFFFF`/`0xFFFF_FFFF`):
    /// cuenta/tamaño/offset reales SOLO en el EOCD64. `claimed` fuerza una
    /// cuenta mentirosa de 64 bits en el EOCD64.
    Zip64 { claimed: Option<u64> },
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
    comment: Vec<u8>,
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

    /// Entrada con `method`/`flags` crudos: cifrado simulado (bit 0),
    /// métodos no soportados (99 = AES), lo que haga falta romper.
    #[must_use]
    pub fn file_raw(mut self, name: &[u8], data: &[u8], method: u16, flags: u16) -> Self {
        self.entries.push(ZipEntry::Raw {
            name: name.to_vec(),
            data: data.to_vec(),
            method,
            flags,
        });
        self
    }

    /// Archivo `stored` con un extra field CRUDO en su entrada del CENTRAL
    /// directory (#59): 0x7075 Info-ZIP unicode path, zip64 forjado o
    /// basura arbitraria. El local header queda SIN extra.
    #[must_use]
    pub fn file_with_extra(mut self, name: &[u8], data: &[u8], extra: &[u8]) -> Self {
        self.entries.push(ZipEntry::WithExtra {
            name: name.to_vec(),
            data: data.to_vec(),
            extra: extra.to_vec(),
        });
        self
    }

    /// Archivo `deflate` REAL (#59): `deflated` son los bytes YA comprimidos
    /// (el caller los produce con flate2 — esta forja no comprime); crc y
    /// tamaño sin comprimir salen de `uncomp`.
    #[must_use]
    pub fn file_deflate(mut self, name: &[u8], uncomp: &[u8], deflated: &[u8]) -> Self {
        self.entries.push(ZipEntry::Deflate {
            name: name.to_vec(),
            uncomp: uncomp.to_vec(),
            deflated: deflated.to_vec(),
        });
        self
    }

    /// Comentario del EOCD (bytes arbitrarios — incluso firmas EOCD falsas,
    /// el clásico que rompe localizadores ingenuos).
    #[must_use]
    pub fn comment(mut self, bytes: &[u8]) -> Self {
        self.comment = bytes.to_vec();
        self
    }

    /// Los bytes del ZIP completo (local headers + central directory + EOCD).
    #[must_use]
    pub fn build(self) -> Vec<u8> {
        self.build_end(&ZipEnd::Classic { claimed: None })
    }

    /// Como [`Self::build`] pero el EOCD MIENTE: declara `claimed` entradas
    /// (bomba de índice barata: anuncia millones sin pagarlos).
    #[must_use]
    pub fn build_lying_eocd(self, claimed: u16) -> Vec<u8> {
        self.build_end(&ZipEnd::Classic {
            claimed: Some(claimed),
        })
    }

    /// Como [`Self::build`] pero con final zip64 (#59): EOCD64 + locator +
    /// EOCD con marcadores (`0xFFFF`/`0xFFFF_FFFF`) — cuenta/tamaño/offset
    /// reales SOLO en el EOCD64.
    #[must_use]
    pub fn build_zip64(self) -> Vec<u8> {
        self.build_end(&ZipEnd::Zip64 { claimed: None })
    }

    /// Como [`Self::build_zip64`] pero el EOCD64 MIENTE la cuenta (bomba de
    /// índice zip64: el preflight u16 clásico no la veía, #59).
    #[must_use]
    pub fn build_zip64_lying_count(self, claimed: u64) -> Vec<u8> {
        self.build_end(&ZipEnd::Zip64 {
            claimed: Some(claimed),
        })
    }

    #[allow(clippy::cast_possible_truncation)] // fixtures pequeñas por diseño
    fn build_end(self, end: &ZipEnd) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        let real_count = self.entries.len() as u16;
        for entry in &self.entries {
            let ZipWire {
                name,
                payload,
                flags,
                method,
                crc,
                uncomp_len,
                extra,
            } = entry.wire();
            let offset = out.len() as u32;
            let comp_len = payload.len() as u32;
            // Local file header.
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&flags.to_le_bytes());
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&DOS_TIME.to_le_bytes());
            out.extend_from_slice(&DOS_DATE.to_le_bytes());
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&comp_len.to_le_bytes()); // comprimido
            out.extend_from_slice(&uncomp_len.to_le_bytes()); // sin comprimir
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra
            out.extend_from_slice(name);
            out.extend_from_slice(payload);
            // Central directory entry.
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes()); // made by
            central.extend_from_slice(&20u16.to_le_bytes()); // needed
            central.extend_from_slice(&flags.to_le_bytes());
            central.extend_from_slice(&method.to_le_bytes());
            central.extend_from_slice(&DOS_TIME.to_le_bytes());
            central.extend_from_slice(&DOS_DATE.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&comp_len.to_le_bytes());
            central.extend_from_slice(&uncomp_len.to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes()); // comment
            central.extend_from_slice(&0u16.to_le_bytes()); // disk
            central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            let external: u32 = match entry {
                ZipEntry::Dir { .. } => 0x10, // bit de directorio DOS
                _ => 0,
            };
            central.extend_from_slice(&external.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name);
            central.extend_from_slice(extra);
        }
        let cd_offset = out.len() as u64;
        let cd_size = central.len() as u64;
        out.extend_from_slice(&central);
        emit_zip_end(&mut out, end, real_count, cd_offset, cd_size);
        out.extend_from_slice(&(self.comment.len() as u16).to_le_bytes());
        out.extend_from_slice(&self.comment);
        out
    }
}

/// Los campos ya resueltos que una entrada aporta al local header y al CD.
struct ZipWire<'a> {
    name: &'a [u8],
    /// Bytes escritos tras el local header (comprimidos si `Deflate`).
    payload: &'a [u8],
    flags: u16,
    method: u16,
    /// CRC declarado (del contenido SIN comprimir).
    crc: u32,
    /// Tamaño sin comprimir declarado.
    uncomp_len: u32,
    /// Extra field del CD (el local va SIEMPRE sin extra).
    extra: &'a [u8],
}

impl ZipEntry {
    #[allow(clippy::cast_possible_truncation)] // fixtures pequeñas por diseño
    fn wire(&self) -> ZipWire<'_> {
        match self {
            ZipEntry::File {
                name,
                data,
                utf8_flag,
            } => ZipWire {
                name,
                payload: data,
                flags: if *utf8_flag { 1u16 << 11 } else { 0 },
                method: 0,
                crc: crc32(data),
                uncomp_len: data.len() as u32,
                extra: &[],
            },
            ZipEntry::Dir { name } => ZipWire {
                name,
                payload: &[],
                flags: 0,
                method: 0,
                crc: crc32(&[]),
                uncomp_len: 0,
                extra: &[],
            },
            ZipEntry::Raw {
                name,
                data,
                method,
                flags,
            } => ZipWire {
                name,
                payload: data,
                flags: *flags,
                method: *method,
                crc: crc32(data),
                uncomp_len: data.len() as u32,
                extra: &[],
            },
            ZipEntry::WithExtra { name, data, extra } => ZipWire {
                name,
                payload: data,
                flags: 0,
                method: 0,
                crc: crc32(data),
                uncomp_len: data.len() as u32,
                extra,
            },
            ZipEntry::Deflate {
                name,
                uncomp,
                deflated,
            } => ZipWire {
                name,
                payload: deflated,
                flags: 0,
                method: 8,
                crc: crc32(uncomp),
                uncomp_len: uncomp.len() as u32,
                extra: &[],
            },
        }
    }
}

/// Emite el final del zip: EOCD clásico o cadena EOCD64 + locator + EOCD
/// con marcadores (#59). El comentario lo escribe el caller a continuación.
#[allow(clippy::cast_possible_truncation)] // fixtures pequeñas por diseño
fn emit_zip_end(out: &mut Vec<u8>, end: &ZipEnd, real_count: u16, cd_offset: u64, cd_size: u64) {
    match end {
        ZipEnd::Classic { claimed } => {
            let count = claimed.unwrap_or(real_count);
            out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // disco
            out.extend_from_slice(&0u16.to_le_bytes()); // disco del CD
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&(cd_size as u32).to_le_bytes());
            out.extend_from_slice(&(cd_offset as u32).to_le_bytes());
        }
        ZipEnd::Zip64 { claimed } => {
            let count = claimed.unwrap_or(u64::from(real_count));
            let eocd64_pos = out.len() as u64;
            // EOCD64 (56 bytes: tamaño del record = 44, lo que sigue a
            // los 12 primeros).
            out.extend_from_slice(&0x0606_4b50u32.to_le_bytes());
            out.extend_from_slice(&44u64.to_le_bytes()); // size of record
            out.extend_from_slice(&45u16.to_le_bytes()); // made by
            out.extend_from_slice(&45u16.to_le_bytes()); // needed
            out.extend_from_slice(&0u32.to_le_bytes()); // disco
            out.extend_from_slice(&0u32.to_le_bytes()); // disco del CD
            out.extend_from_slice(&count.to_le_bytes()); // en este disco
            out.extend_from_slice(&count.to_le_bytes()); // total
            out.extend_from_slice(&cd_size.to_le_bytes());
            out.extend_from_slice(&cd_offset.to_le_bytes());
            // Locator del EOCD64 (20 bytes, JUSTO antes del EOCD).
            out.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes()); // disco del EOCD64
            out.extend_from_slice(&eocd64_pos.to_le_bytes());
            out.extend_from_slice(&1u32.to_le_bytes()); // discos totales
            // EOCD clásico con MARCADORES: los valores reales viven en
            // el EOCD64.
            out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // disco
            out.extend_from_slice(&0u16.to_le_bytes()); // disco del CD
            out.extend_from_slice(&u16::MAX.to_le_bytes());
            out.extend_from_slice(&u16::MAX.to_le_bytes());
            out.extend_from_slice(&u32::MAX.to_le_bytes());
            out.extend_from_slice(&u32::MAX.to_le_bytes());
        }
    }
}

enum TarEntry {
    File {
        name: Vec<u8>,
        data: Vec<u8>,
    },
    Dir {
        name: Vec<u8>,
    },
    Symlink {
        name: Vec<u8>,
        target: Vec<u8>,
    },
    /// Archivo con nombre LARGO vía GNU longname (#60): entrada `L`
    /// («`././@LongLink`», datos = nombre real + NUL) seguida del archivo con
    /// el nombre TRUNCADO a 100 en su header.
    GnuLongName {
        name: Vec<u8>,
        data: Vec<u8>,
    },
    /// Archivo con override pax `path=` (#60): entrada `x` con el record
    /// pax seguida del archivo con nombre placeholder.
    PaxPath {
        name: Vec<u8>,
        data: Vec<u8>,
    },
    /// Entrada CRUDA con typeflag arbitrario (#60): pins de metadatos que
    /// el iterador del crate `tar` consume o debe filtrar (`g` =
    /// `pax_global_header`, H5).
    Raw {
        typeflag: u8,
        name: Vec<u8>,
        data: Vec<u8>,
    },
}

/// Forja de bytes tar (ustar plano + GNU longname y pax `path=` desde #60).
/// Un header ustar solo admite 100 bytes de nombre: los largos van por
/// [`TarSmith::file_gnu_longname`] / [`TarSmith::file_pax_path`]; `file`
/// con nombre >100 sigue PANICANDO (contrato de forja explícito).
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

    /// Archivo con nombre de CUALQUIER longitud vía GNU longname (#60, H6):
    /// entrada `L` con el nombre real como datos + el archivo con el nombre
    /// truncado a 100 en su header ustar — como GNU tar de verdad.
    #[must_use]
    pub fn file_gnu_longname(mut self, name: &[u8], data: &[u8]) -> Self {
        self.entries.push(TarEntry::GnuLongName {
            name: name.to_vec(),
            data: data.to_vec(),
        });
        self
    }

    /// Archivo con override pax `path=` (#60, H6): entrada `x` con el record
    /// `LEN path=NOMBRE\n` (bytes crudos — pax real exige UTF-8, los tars
    /// hostiles no) + el archivo con nombre placeholder.
    #[must_use]
    pub fn file_pax_path(mut self, name: &[u8], data: &[u8]) -> Self {
        self.entries.push(TarEntry::PaxPath {
            name: name.to_vec(),
            data: data.to_vec(),
        });
        self
    }

    /// Entrada cruda con `typeflag` arbitrario (#60): p. ej. `b'g'` para
    /// pinear el filtro de `pax_global_header` (H5).
    #[must_use]
    pub fn entry_raw(mut self, typeflag: u8, name: &[u8], data: &[u8]) -> Self {
        self.entries.push(TarEntry::Raw {
            typeflag,
            name: name.to_vec(),
            data: data.to_vec(),
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
            match entry {
                TarEntry::File { name, data } => emit_tar(&mut out, name, data, b'0', &[]),
                TarEntry::Dir { name } => emit_tar(&mut out, name, &[], b'5', &[]),
                TarEntry::Symlink { name, target } => emit_tar(&mut out, name, &[], b'2', target),
                TarEntry::GnuLongName { name, data } => {
                    // GNU longname: entrada `L` con el nombre real + NUL como
                    // datos; el header del archivo lleva el nombre truncado.
                    let mut long = name.clone();
                    long.push(0);
                    emit_tar(&mut out, b"././@LongLink", &long, b'L', &[]);
                    emit_tar(&mut out, &name[..name.len().min(100)], data, b'0', &[]);
                }
                TarEntry::PaxPath { name, data } => {
                    // Record pax `LEN path=NOMBRE\n` con LEN = longitud TOTAL
                    // del record (dígitos incluidos) — el clásico cálculo
                    // iterativo del formato.
                    let base = " path=".len() + name.len() + 1;
                    let mut len = base + 1;
                    while len.to_string().len() + base != len {
                        len = len.to_string().len() + base;
                    }
                    let mut record = format!("{len} path=").into_bytes();
                    record.extend_from_slice(name);
                    record.push(b'\n');
                    emit_tar(&mut out, b"PaxHeader/x", &record, b'x', &[]);
                    emit_tar(&mut out, b"placeholder", data, b'0', &[]);
                }
                TarEntry::Raw {
                    typeflag,
                    name,
                    data,
                } => emit_tar(&mut out, name, data, *typeflag, &[]),
            }
        }
        out.extend(std::iter::repeat_n(0u8, 1024));
        out
    }
}

/// Emite UN header ustar de 512 + datos + padding. `name`/`link` ≤ 100
/// bytes (panic: contrato de forja, ver doc de [`TarSmith`]).
fn emit_tar(out: &mut Vec<u8>, name: &[u8], data: &[u8], typeflag: u8, link: &[u8]) {
    assert!(
        name.len() <= 100 && link.len() <= 100,
        "TarSmith no forja headers con nombre >100 bytes (usa file_gnu_longname/file_pax_path)"
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
    // Magic POSIX («ustar\0» + «00») también en la entrada L: GNU tar real
    // emite el magic old-GNU («ustar  \0») — tar-rs honra la L con ambos
    // (nit del audit #60; un parser que exija el magic GNU divergiría).
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
    fn zip64_estructura_coherente() {
        let z = ZipSmith::new().file(b"a", b"data").build_zip64();
        // EOCD final con MARCADORES.
        let eocd = &z[z.len() - 22..];
        assert_eq!(&eocd[..4], b"PK\x05\x06");
        assert_eq!(u16::from_le_bytes([eocd[10], eocd[11]]), u16::MAX);
        assert_eq!(
            u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]),
            u32::MAX
        );
        // Locator 20 bytes antes: apunta a un EOCD64 con la cuenta real.
        let loc = &z[z.len() - 42..z.len() - 22];
        assert_eq!(&loc[..4], b"PK\x06\x07");
        let eocd64_pos =
            usize::try_from(u64::from_le_bytes(loc[8..16].try_into().unwrap())).unwrap();
        assert_eq!(&z[eocd64_pos..eocd64_pos + 4], b"PK\x06\x06");
        let count = u64::from_le_bytes(z[eocd64_pos + 32..eocd64_pos + 40].try_into().unwrap());
        assert_eq!(count, 1);
        // La cuenta mentirosa vive en el EOCD64, no en el EOCD.
        let liar = ZipSmith::new()
            .file(b"a", b"d")
            .build_zip64_lying_count(9_000_000);
        let loc = &liar[liar.len() - 42..liar.len() - 22];
        let pos = usize::try_from(u64::from_le_bytes(loc[8..16].try_into().unwrap())).unwrap();
        assert_eq!(
            u64::from_le_bytes(liar[pos + 32..pos + 40].try_into().unwrap()),
            9_000_000
        );
    }

    #[test]
    fn zip_extra_solo_en_el_cd() {
        let extra = [0x75u8, 0x70, 0x03, 0x00, 0x01, 0x02, 0x03]; // id 0x7075
        let z = ZipSmith::new().file_with_extra(b"n", b"d", &extra).build();
        // Local header: extra_len = 0 (el extra vive SOLO en el CD).
        assert_eq!(u16::from_le_bytes([z[28], z[29]]), 0);
        // CD: extra_len = 7 y los bytes están tras el nombre.
        let cd = z.windows(4).position(|w| w == b"PK\x01\x02").expect("cd");
        assert_eq!(u16::from_le_bytes([z[cd + 30], z[cd + 31]]), 7);
        assert_eq!(&z[cd + 46 + 1..cd + 46 + 1 + 7], &extra);
    }

    #[test]
    fn zip_deflate_declara_tamanos_reales() {
        // "deflated" simulado más corto que el contenido: comp != uncomp.
        let z = ZipSmith::new()
            .file_deflate(b"f", b"0123456789", b"XYZ")
            .build();
        let cd = z.windows(4).position(|w| w == b"PK\x01\x02").expect("cd");
        assert_eq!(u16::from_le_bytes([z[cd + 10], z[cd + 11]]), 8, "method");
        let comp = u32::from_le_bytes(z[cd + 20..cd + 24].try_into().unwrap());
        let uncomp = u32::from_le_bytes(z[cd + 24..cd + 28].try_into().unwrap());
        assert_eq!((comp, uncomp), (3, 10));
        let crc = u32::from_le_bytes(z[cd + 16..cd + 20].try_into().unwrap());
        assert_eq!(crc, crc32(b"0123456789"), "crc del contenido SIN comprimir");
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
    #[should_panic(expected = "no forja headers con nombre >100")]
    fn tar_nombre_largo_panica() {
        let _ = TarSmith::new().file(&[b'a'; 101], b"").build();
    }

    /// #60 (INFO del audit): el LEN del record pax en las transiciones de
    /// dígitos — base 97 → LEN 99 (2 dígitos), base 98 → 101 (salta el 100
    /// imposible), base 99 → 102. El record emitido mide EXACTAMENTE su LEN.
    #[test]
    fn pax_len_en_transiciones_de_digitos() {
        for name_len in [91usize, 92, 93, 13] {
            let name = vec![b'n'; name_len];
            let tar = TarSmith::new().file_pax_path(&name, b"d").build();
            // La entrada x es el primer header: sus datos empiezan en 512.
            let size = usize::from_str_radix(
                std::str::from_utf8(&tar[124..135])
                    .unwrap()
                    .trim_end_matches('\0')
                    .trim(),
                8,
            )
            .expect("size octal");
            let record = &tar[512..512 + size];
            let espacio = record.iter().position(|&b| b == b' ').expect("LEN espacio");
            let len: usize = std::str::from_utf8(&record[..espacio])
                .unwrap()
                .parse()
                .expect("LEN decimal");
            assert_eq!(
                len,
                record.len(),
                "name_len={name_len}: LEN == longitud real"
            );
        }
    }
}
