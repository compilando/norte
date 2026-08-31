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
    /// #100.2: la ÚLTIMA entrada del CD declara este `comment_len` sin
    /// escribir sus bytes — un CD truncado a mitad del comentario por-entrada.
    cd_comment_len_lie: Option<u16>,
    /// Fecha DOS de TODAS las entradas. [`ZipSmith::undated`] la pone a cero,
    /// que es un par INVÁLIDO (mes 0, día 0) y no una fecha de 1980.
    dos_date: Option<u16>,
}

impl ZipSmith {
    /// Forja vacía.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Zip cuyas entradas NO llevan fecha utilizable: el par DOS sale a cero,
    /// que es inválido (mes 0, día 0) y que un lector honrado tiene que
    /// reportar como «no hay fecha», jamás como 1980-00-00.
    ///
    /// Es el caso de un zip escrito por una herramienta que deja el campo en
    /// blanco, y el único con el que una comparación contra un archivo puede
    /// llegar a `CompareConfidence::Unknown` por la vía de la fecha.
    ///
    /// ```
    /// let bytes = norte_testkit::ZipSmith::new().undated().file(b"a", b"x").build();
    /// // Offset 10 del local header: dos_time (u16) y después dos_date (u16).
    /// assert_eq!(&bytes[10..14], &[0, 0, 0, 0]);
    /// ```
    #[must_use]
    pub fn undated(mut self) -> Self {
        self.dos_date = Some(0);
        self
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

    /// La ÚLTIMA entrada del CD declara `len` bytes de comentario por-entrada
    /// SIN escribirlos: el `cd_size` del EOCD no los cubre, así el walk del CD
    /// se queda corto a mitad del comentario (#100.2 — pin de
    /// `skipped != comment_len → Corrupt`). Sin entradas es un no-op.
    #[must_use]
    pub fn cd_comment_len_lie(mut self, len: u16) -> Self {
        self.cd_comment_len_lie = Some(len);
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
        let last_idx = self.entries.len().wrapping_sub(1);
        let dos_date = self.dos_date.unwrap_or(DOS_DATE);
        for (idx, entry) in self.entries.iter().enumerate() {
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
            out.extend_from_slice(&dos_date.to_le_bytes());
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
            central.extend_from_slice(&dos_date.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&comp_len.to_le_bytes());
            central.extend_from_slice(&uncomp_len.to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            // Comentario por-entrada: 0 salvo la mentira del #100.2 en la
            // última entrada (declara bytes que NO se escriben en el CD).
            let entry_comment_len = if idx == last_idx {
                self.cd_comment_len_lie.unwrap_or(0)
            } else {
                0
            };
            central.extend_from_slice(&entry_comment_len.to_le_bytes()); // comment
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

// --- RAR5 -----------------------------------------------------------------

/// Entero de longitud variable de RAR5: 7 bits por byte, bit alto = «sigue».
fn vint(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = u8::try_from(n & 0x7f).expect("7 bits caben en u8");
        n >>= 7;
        if n == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

/// El ejecutable `7z` si está en `PATH`. Los tests que necesitan un delegado
/// real se retiran diciéndolo cuando devuelve `None`.
///
/// ```
/// // En una máquina sin 7z instalado esto es `None`, y eso no es un fallo.
/// let _ = norte_testkit::which_7z();
/// ```
#[must_use]
pub fn which_7z() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for exe in ["7z", "7zz"] {
            let candidate = dir.join(exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Una entrada de la forja RAR5.
struct RarEntry {
    name: Vec<u8>,
    content: Vec<u8>,
    is_dir: bool,
}

/// Forja de bytes RAR5 con entradas ALMACENADAS (método 0), hermana de
/// [`ZipSmith`] y [`TarSmith`].
///
/// Existe porque el compresor de RAR es la mitad no libre: nada en este árbol
/// puede producir un `.rar` comprimido, así que sin esto no hay fixture
/// ninguna — ni corpus hostil, ni suite contractual. El CONTENEDOR está
/// documentado y meter bytes crudos dentro no toca el algoritmo propietario.
///
/// ```
/// let bytes = norte_testkit::RarSmith::new()
///     .file(b"docs/hola.txt", b"hola")
///     .build();
/// assert_eq!(&bytes[..8], b"Rar!\x1a\x07\x01\x00");
/// ```
#[derive(Default)]
pub struct RarSmith {
    entries: Vec<RarEntry>,
    mtime: u32,
}

impl RarSmith {
    /// Forja vacía. `mtime` fijo (2021-01-14) para determinismo total.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            mtime: 0x6000_0000,
        }
    }

    /// Fichero almacenado. `name` va en BYTES crudos: un nombre no-UTF8 es
    /// exactamente el caso que hay que poder forjar (regla dura 1).
    #[must_use]
    pub fn file(mut self, name: &[u8], content: &[u8]) -> Self {
        self.entries.push(RarEntry {
            name: name.to_vec(),
            content: content.to_vec(),
            is_dir: false,
        });
        self
    }

    /// Entrada de directorio explícita (sin datos).
    #[must_use]
    pub fn dir(mut self, name: &[u8]) -> Self {
        self.entries.push(RarEntry {
            name: name.to_vec(),
            content: Vec::new(),
            is_dir: true,
        });
        self
    }

    /// Los bytes del `.rar`.
    #[must_use]
    pub fn build(self) -> Vec<u8> {
        let mut out = Vec::from(*b"Rar!\x1a\x07\x01\x00");
        // Cabecera principal: head_type 1, sin flags, ArchiveFlags = 0.
        out.extend_from_slice(&rar_block(1, 0, &vint(0), &[]));
        for e in &self.entries {
            out.extend_from_slice(&self.rar_file_block(e));
        }
        // Fin de archivo: head_type 5, EndFlags = 0.
        out.extend_from_slice(&rar_block(5, 0, &vint(0), &[]));
        out
    }

    /// Cabecera de fichero (`head_type` 2) seguida de sus datos crudos.
    fn rar_file_block(&self, e: &RarEntry) -> Vec<u8> {
        // FileFlags: 0x0001 directorio | 0x0002 mtime presente | 0x0004 crc.
        let file_flags: u64 = u64::from(e.is_dir) | 0x0002 | 0x0004;
        let attrs: u64 = if e.is_dir { 0x10 } else { 0x20 };
        let mut body = vint(file_flags);
        body.extend_from_slice(&vint(e.content.len() as u64));
        body.extend_from_slice(&vint(attrs));
        body.extend_from_slice(&self.mtime.to_le_bytes());
        body.extend_from_slice(&crc32(&e.content).to_le_bytes());
        // CompressionInfo: versión 0, método 0 (almacenado), diccionario 0.
        body.extend_from_slice(&vint(0));
        // HostOS: 1 = unix.
        body.extend_from_slice(&vint(1));
        body.extend_from_slice(&vint(e.name.len() as u64));
        body.extend_from_slice(&e.name);

        // head_flags 0x0002 = el bloque declara DataSize (los bytes que lo
        // siguen). Un directorio no lleva datos y no lo declara.
        let mut block = if e.is_dir {
            rar_block(2, 0, &body, &[])
        } else {
            rar_block(2, 0x0002, &body, &e.content)
        };
        block.extend_from_slice(&e.content);
        block
    }

    /// Los bytes de un **RAR4**, el formato viejo, con los nombres en BYTES
    /// CRUDOS (#223).
    ///
    /// RAR5 guarda los nombres en UTF-8 por formato, así que con `build` no se
    /// puede escribir el caso que de verdad hay ahí fuera: **un archivo hecho
    /// en una máquina con code page OEM** (CP437, CP866, CP1251…), que es lo
    /// que contiene una década de descargas. RAR4 sí lo permite: sin el flag
    /// `LHD_UNICODE` (0x0200) el nombre viaja tal cual, y eso es lo que forja
    /// esto.
    ///
    /// La issue daba por hecho que forjar RAR4 «empieza a parecerse a
    /// reimplementar el formato que deliberadamente no implementamos», y por
    /// eso proponía meter un binario de terceros en el repo. No hace falta: lo
    /// que se forja aquí es el CONTENEDOR con una entrada ALMACENADA, igual
    /// que en RAR5 — no se toca el algoritmo propietario, que es la parte que
    /// norte no implementa ni implementará. Y sale mejor que un binario: es
    /// determinista, no plantea preguntas de licencia ni de procedencia, y
    /// puede llevar cualquier nombre del corpus hostil.
    ///
    /// **Verificado contra `unrar` 7.23 y `7z`**, que es lo que lo hace un
    /// fixture y no una suposición. De paso contesta las tres preguntas que la
    /// issue dejaba abiertas: `7z -slt` imprime los bytes OEM CRUDOS; `unrar`
    /// NO —los mapea a un rango de uso privado (U+E0xx) precedido de U+FFFE—;
    /// y ninguno de los dos TRUNCA el nombre, que era el fallo medido para los
    /// RAR5 no-UTF8.
    ///
    /// Solo entradas de fichero: un RAR4 con directorios explícitos no aporta
    /// nada que RAR5 no cubra ya.
    ///
    /// ```
    /// // `папка.txt` en CP866, que es el nombre ruso clásico de una máquina DOS.
    /// let nombre = b"\xaf\xa0\xaf\xaa\xa0.txt";
    /// let bytes = norte_testkit::RarSmith::new()
    ///     .file(nombre, b"hola")
    ///     .build_rar4();
    /// assert_eq!(&bytes[..7], b"Rar!\x1a\x07\x00");
    /// // El nombre está DENTRO, byte a byte y sin transcodificar.
    /// assert!(bytes.windows(nombre.len()).any(|w| w == nombre));
    /// ```
    #[must_use]
    pub fn build_rar4(self) -> Vec<u8> {
        // El marcador de RAR4 acaba en 0x00; el de RAR5, en 0x01 0x00. Es lo
        // primero que mira cualquier lector para saber con qué habla.
        let mut out = Vec::from(*b"Rar!\x1a\x07\x00");
        out.extend_from_slice(&rar4_main_head());
        for e in self.entries.iter().filter(|e| !e.is_dir) {
            out.extend_from_slice(&rar4_file_head(&e.name, &e.content));
        }
        out
    }
}

/// La cabecera principal de un RAR4 (`HEAD_TYPE` 0x73), de trece bytes.
fn rar4_main_head() -> Vec<u8> {
    let mut cuerpo = vec![0x73, 0x00, 0x00, 13, 0x00];
    cuerpo.extend_from_slice(&[0u8; 6]); // RESERVED1(2) + RESERVED2(4)
    rar4_con_crc(&cuerpo)
}

/// Una cabecera de fichero RAR4 (`HEAD_TYPE` 0x74) seguida de sus datos.
///
/// Método 0x30 = ALMACENADO, que es lo único que este árbol puede escribir. Sin
/// `LHD_UNICODE` (0x0200) a propósito: el nombre son los bytes que se le pasen.
fn rar4_file_head(nombre: &[u8], datos: &[u8]) -> Vec<u8> {
    let tam = u16::try_from(32 + nombre.len()).unwrap_or(u16::MAX);
    let n = u32::try_from(datos.len()).unwrap_or(u32::MAX);
    let mut cuerpo = vec![0x74];
    // LHD_LONG_BLOCK (0x8000): el bloque va seguido de sus datos.
    cuerpo.extend_from_slice(&0x8000u16.to_le_bytes());
    cuerpo.extend_from_slice(&tam.to_le_bytes());
    cuerpo.extend_from_slice(&n.to_le_bytes()); // PACK_SIZE
    cuerpo.extend_from_slice(&n.to_le_bytes()); // UNP_SIZE
    // HOST_OS 0x02 = Win32, que es de donde salen las code pages OEM.
    cuerpo.push(0x02);
    cuerpo.extend_from_slice(&crc32(datos).to_le_bytes());
    cuerpo.extend_from_slice(&0x5000_0000u32.to_le_bytes()); // FTIME, fijo
    cuerpo.push(20); // UNP_VER 2.0
    cuerpo.push(0x30); // METHOD: almacenado
    cuerpo.extend_from_slice(
        &u16::try_from(nombre.len())
            .unwrap_or(u16::MAX)
            .to_le_bytes(),
    );
    cuerpo.extend_from_slice(&0x20u32.to_le_bytes()); // ATTR
    cuerpo.extend_from_slice(nombre);
    let mut bloque = rar4_con_crc(&cuerpo);
    bloque.extend_from_slice(datos);
    bloque
}

/// Antepone el `HEAD_CRC` de RAR4: los DOS BYTES BAJOS del CRC32 de la
/// cabecera, contando desde `HEAD_TYPE`.
fn rar4_con_crc(cuerpo: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(cuerpo.len() + 2);
    // El truncado es EL formato, no un descuido: RAR4 guarda dos bytes donde
    // hay un CRC32, y son los bajos. `unrar` valida exactamente esos.
    #[allow(clippy::cast_possible_truncation)]
    let bajos = crc32(cuerpo) as u16;
    out.extend_from_slice(&bajos.to_le_bytes());
    out.extend_from_slice(cuerpo);
    out
}

/// Un bloque RAR5: `crc32(len ++ inner) ++ len ++ inner`, donde `inner` es
/// `head_type ++ head_flags ++ [data_size] ++ body`. El CRC cubre la longitud
/// y el interior, no los datos que van detrás del bloque.
fn rar_block(head_type: u64, head_flags: u64, body: &[u8], data: &[u8]) -> Vec<u8> {
    let mut inner = vint(head_type);
    inner.extend_from_slice(&vint(head_flags));
    if head_flags & 0x0002 != 0 {
        inner.extend_from_slice(&vint(data.len() as u64));
    }
    inner.extend_from_slice(body);

    let mut hdr = vint(inner.len() as u64);
    hdr.extend_from_slice(&inner);

    let mut out = Vec::from(crc32(&hdr).to_le_bytes());
    out.extend_from_slice(&hdr);
    out
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

    // --- RarSmith (ADR 0018 / item 11 del roadmap) -------------------------

    /// El writer produce un archivo que el DELEGADO REAL sabe leer. Un writer
    /// «correcto» según nuestra propia lectura no demuestra nada.
    #[test]
    fn un_delegado_real_lista_lo_que_forjamos() {
        const CRUDO: &[u8] = b"cp437-\xa4\xa5.txt";
        let Some(sevenz) = which_7z() else {
            eprintln!("sin 7z instalado: test retirado");
            return;
        };
        let bytes = RarSmith::new()
            .file(b"hello.txt", b"hola norte\n")
            .file("\u{f1}and\u{fa}.txt".as_bytes(), b"utf8\n")
            .file(CRUDO, b"bytes\n")
            .build();
        let dir = std::env::temp_dir().join(format!("norte-rarsmith-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tempdir");
        let path = dir.join("t.rar");
        std::fs::write(&path, &bytes).expect("escribe");

        let out = std::process::Command::new(sevenz)
            .args(["l", "-slt", "-p", "--"])
            .arg(&path)
            .output()
            .expect("7z corre");
        std::fs::remove_dir_all(&dir).ok();
        assert!(out.status.success(), "7z falló: {out:?}");
        // Los BYTES crudos del nombre no-UTF8 sobreviven al listado de 7z.
        assert!(
            out.stdout.windows(CRUDO.len()).any(|w| w == CRUDO),
            "el nombre crudo no aparece en el listado de 7z: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    #[test]
    fn vint_codifica_multibyte() {
        assert_eq!(vint(0), vec![0x00]);
        assert_eq!(vint(0x7f), vec![0x7f]);
        assert_eq!(vint(0x80), vec![0x80, 0x01]);
        assert_eq!(vint(0x3fff), vec![0xff, 0x7f]);
    }

    #[test]
    fn la_firma_es_rar5_y_el_contenido_va_crudo() {
        let bytes = RarSmith::new().file(b"a.txt", b"CRUDO").build();
        assert_eq!(&bytes[..8], b"Rar!\x1a\x07\x01\x00");
        // Método 0 = almacenado: el contenido está literalmente ahí dentro.
        assert!(
            bytes.windows(5).any(|w| w == b"CRUDO"),
            "una entrada almacenada no comprime nada"
        );
    }
}
