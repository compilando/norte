//! Escritor de zip INCREMENTAL: entrada a entrada, trozo a trozo.
//!
//! No escribe en ningún sitio — produce bytes en un búfer que el llamante
//! drena y manda a donde sea (un `ByteSink` remoto, un fichero local). Es lo
//! que permite empaquetar contra un destino asíncrono sin tener el archivo
//! entero en memoria, y de paso lo hace probable sin I/O.
//!
//! Solo lo que este repositorio sabe leer: `store` y `deflate`, sin cifrado.

use std::io::Write as _;

use super::{PackEntry, PackError};

/// Firma del header local de una entrada.
const LOCAL_SIG: u32 = 0x0403_4b50;
/// Firma de una entrada del directorio central.
const CD_SIG: u32 = 0x0201_4b50;
/// Firma del End Of Central Directory.
const EOCD_SIG: u32 = 0x0605_4b50;
/// Firma del EOCD de zip64.
const EOCD64_SIG: u32 = 0x0606_4b50;
/// Firma del localizador del EOCD de zip64.
const EOCD64_LOC_SIG: u32 = 0x0706_4b50;
/// Firma del descriptor de datos (opcional, pero todo el mundo la escribe).
const DD_SIG: u32 = 0x0807_4b50;

/// Bit 3 de los flags: tamaños y CRC van DETRÁS de los datos, en un descriptor.
///
/// Es lo que hace posible escribir sin saber de antemano cuánto va a ocupar la
/// entrada comprimida, que es justo lo que un escritor incremental no sabe.
const FLAG_DATA_DESCRIPTOR: u16 = 1 << 3;
/// Bit 11: el nombre está en UTF-8.
const FLAG_UTF8: u16 = 1 << 11;

/// Método `store` (sin comprimir).
const METHOD_STORE: u16 = 0;
/// Método `deflate`.
const METHOD_DEFLATE: u16 = 8;

/// Versión mínima para extraer: 2.0 (deflate). Con zip64 sube a 4.5.
const VERSION_BASE: u16 = 20;
/// Versión mínima cuando la entrada necesita zip64.
const VERSION_ZIP64: u16 = 45;

/// A partir de aquí un campo de 32 bits no vale y hace falta zip64.
const U32_MAX: u64 = u32::MAX as u64;

/// Una entrada ya escrita, para el directorio central.
struct Written {
    name: Vec<u8>,
    flags: u16,
    method: u16,
    dos_time: u16,
    dos_date: u16,
    crc: u32,
    comp_size: u64,
    uncomp_size: u64,
    offset: u64,
    external_attrs: u32,
}

/// Lo que se está comprimiendo ahora mismo.
enum Cuerpo {
    Store,
    Deflate(Box<flate2::write::DeflateEncoder<Vec<u8>>>),
}

/// Entrada en curso.
struct EnCurso {
    name: Vec<u8>,
    flags: u16,
    method: u16,
    dos_time: u16,
    dos_date: u16,
    offset: u64,
    external_attrs: u32,
    crc: flate2::Crc,
    uncomp: u64,
    comp: u64,
    cuerpo: Cuerpo,
}

/// Escritor incremental de zip.
///
/// El ciclo es `begin(entry)` → `data(chunk)`* → `end()` por cada entrada, y
/// `finish()` al terminar. Entre llamada y llamada, [`ZipWriter::take`] saca
/// lo producido: quien empaqueta lo drena y lo manda, así que ni el archivo ni
/// una entrada entera viven en memoria.
pub struct ZipWriter {
    out: Vec<u8>,
    /// Bytes ya ENTREGADOS por `take`, que es lo que fija los offsets del
    /// directorio central: `out` se vacía, la cuenta no.
    entregados: u64,
    hechas: Vec<Written>,
    curso: Option<EnCurso>,
    nivel: u32,
}

impl ZipWriter {
    /// Un escritor con el nivel de compresión dado (0 = `store`).
    #[must_use]
    pub fn new(nivel: u32) -> Self {
        Self {
            out: Vec::new(),
            entregados: 0,
            hechas: Vec::new(),
            curso: None,
            nivel,
        }
    }

    /// Bytes producidos hasta ahora. Se vacía el búfer: el llamante es quien
    /// los guarda.
    pub fn take(&mut self) -> Vec<u8> {
        let v = std::mem::take(&mut self.out);
        self.entregados += v.len() as u64;
        v
    }

    /// Cuánto se ha producido en total (entregado + pendiente).
    fn pos(&self) -> u64 {
        self.entregados + self.out.len() as u64
    }

    /// Abre una entrada.
    ///
    /// # Errors
    ///
    /// [`PackError::Nombre`] si el nombre no cabe en los 16 bits del formato,
    /// o si ya hay una entrada abierta.
    pub fn begin(&mut self, entry: &PackEntry) -> Result<(), PackError> {
        if self.curso.is_some() {
            return Err(PackError::Estado);
        }
        let mut name = entry.name.clone();
        if entry.dir && !name.ends_with(b"/") {
            // La barra final ES lo que dice que es un directorio en zip.
            name.push(b'/');
        }
        if u16::try_from(name.len()).is_err() {
            return Err(PackError::Nombre);
        }
        // **Bit 11 solo si el nombre ES UTF-8** (regla 1). Nuestro lector se
        // queda los bytes crudos y no mira el bit, así que el round-trip es
        // exacto de cualquier forma; el bit se escribe para los OTROS
        // programas, que sí decodifican por él — y ponerlo sobre un nombre que
        // no es UTF-8 convertiría el nombre del usuario en caracteres de
        // reemplazo en cualquier unzip del mundo.
        let utf8 = std::str::from_utf8(&name).is_ok();
        let method = if entry.dir || self.nivel == 0 {
            METHOD_STORE
        } else {
            METHOD_DEFLATE
        };
        let flags = FLAG_DATA_DESCRIPTOR | if utf8 { FLAG_UTF8 } else { 0 };
        let (dos_time, dos_date) = dos_datetime(entry.mtime_ms);
        let offset = self.pos();

        put_u32(&mut self.out, LOCAL_SIG);
        put_u16(&mut self.out, VERSION_BASE);
        put_u16(&mut self.out, flags);
        put_u16(&mut self.out, method);
        put_u16(&mut self.out, dos_time);
        put_u16(&mut self.out, dos_date);
        // CRC y tamaños van a cero: los lleva el descriptor de datos, que es
        // lo que el bit 3 anuncia.
        put_u32(&mut self.out, 0);
        put_u32(&mut self.out, 0);
        put_u32(&mut self.out, 0);
        put_u16(&mut self.out, u16::try_from(name.len()).unwrap_or(u16::MAX));
        put_u16(&mut self.out, 0);
        self.out.extend_from_slice(&name);

        let cuerpo = if method == METHOD_DEFLATE {
            Cuerpo::Deflate(Box::new(flate2::write::DeflateEncoder::new(
                Vec::new(),
                flate2::Compression::new(self.nivel),
            )))
        } else {
            Cuerpo::Store
        };
        self.curso = Some(EnCurso {
            name,
            flags,
            method,
            dos_time,
            dos_date,
            offset,
            external_attrs: external_attrs(entry),
            crc: flate2::Crc::new(),
            uncomp: 0,
            comp: 0,
            cuerpo,
        });
        Ok(())
    }

    /// Añade datos a la entrada abierta.
    ///
    /// # Errors
    ///
    /// [`PackError::Estado`] sin entrada abierta, [`PackError::Io`] si el
    /// compresor falla.
    pub fn data(&mut self, chunk: &[u8]) -> Result<(), PackError> {
        let curso = self.curso.as_mut().ok_or(PackError::Estado)?;
        curso.crc.update(chunk);
        curso.uncomp += chunk.len() as u64;
        match &mut curso.cuerpo {
            Cuerpo::Store => {
                curso.comp += chunk.len() as u64;
                self.out.extend_from_slice(chunk);
            }
            Cuerpo::Deflate(enc) => {
                enc.write_all(chunk).map_err(|_| PackError::Io)?;
                // Se drena lo que el compresor lleve producido en vez de
                // esperar al `finish`: si no, una entrada de un giga vive
                // entera en el búfer del encoder.
                let listo = std::mem::take(enc.get_mut());
                curso.comp += listo.len() as u64;
                self.out.extend_from_slice(&listo);
            }
        }
        Ok(())
    }

    /// Cierra la entrada abierta y escribe su descriptor de datos.
    ///
    /// # Errors
    ///
    /// [`PackError::Estado`] sin entrada abierta, [`PackError::Io`] si el
    /// compresor falla al terminar.
    pub fn end(&mut self) -> Result<(), PackError> {
        let mut curso = self.curso.take().ok_or(PackError::Estado)?;
        if let Cuerpo::Deflate(enc) = &mut curso.cuerpo {
            let cola = enc.try_finish().map(|()| std::mem::take(enc.get_mut()));
            let cola = cola.map_err(|_| PackError::Io)?;
            curso.comp += cola.len() as u64;
            self.out.extend_from_slice(&cola);
        }
        let crc = std::mem::replace(&mut curso.crc, flate2::Crc::new()).sum();
        let zip64 = curso.comp > U32_MAX || curso.uncomp > U32_MAX;
        put_u32(&mut self.out, DD_SIG);
        put_u32(&mut self.out, crc);
        if zip64 {
            put_u64(&mut self.out, curso.comp);
            put_u64(&mut self.out, curso.uncomp);
        } else {
            put_u32(&mut self.out, u32::try_from(curso.comp).unwrap_or(u32::MAX));
            put_u32(
                &mut self.out,
                u32::try_from(curso.uncomp).unwrap_or(u32::MAX),
            );
        }
        self.hechas.push(Written {
            name: curso.name,
            flags: curso.flags,
            method: curso.method,
            dos_time: curso.dos_time,
            dos_date: curso.dos_date,
            crc,
            comp_size: curso.comp,
            uncomp_size: curso.uncomp,
            offset: curso.offset,
            external_attrs: curso.external_attrs,
        });
        Ok(())
    }

    /// Escribe el directorio central y el EOCD. Después de esto el archivo
    /// está completo.
    ///
    /// # Errors
    ///
    /// [`PackError::Estado`] si queda una entrada abierta.
    pub fn finish(&mut self) -> Result<(), PackError> {
        if self.curso.is_some() {
            return Err(PackError::Estado);
        }
        let cd_offset = self.pos();
        let hechas = std::mem::take(&mut self.hechas);
        for e in &hechas {
            self.escribe_cd(e);
        }
        let cd_size = self.pos() - cd_offset;
        let n = hechas.len();
        // Zip64 cuando algo no cabe en 32 bits: el número de entradas, el
        // tamaño del directorio, o dónde empieza. Un offset truncado en
        // silencio es un archivo corrupto que ABRE, que es peor que uno que no.
        let necesita_64 = n > usize::from(u16::MAX)
            || cd_size > U32_MAX
            || cd_offset > U32_MAX
            || hechas
                .iter()
                .any(|e| e.comp_size > U32_MAX || e.uncomp_size > U32_MAX || e.offset > U32_MAX);
        if necesita_64 {
            let eocd64 = self.pos();
            put_u32(&mut self.out, EOCD64_SIG);
            put_u64(&mut self.out, 44); // tamaño del resto de ESTE registro
            put_u16(&mut self.out, VERSION_ZIP64);
            put_u16(&mut self.out, VERSION_ZIP64);
            put_u32(&mut self.out, 0);
            put_u32(&mut self.out, 0);
            put_u64(&mut self.out, n as u64);
            put_u64(&mut self.out, n as u64);
            put_u64(&mut self.out, cd_size);
            put_u64(&mut self.out, cd_offset);
            put_u32(&mut self.out, EOCD64_LOC_SIG);
            put_u32(&mut self.out, 0);
            put_u64(&mut self.out, eocd64);
            put_u32(&mut self.out, 1);
        }
        put_u32(&mut self.out, EOCD_SIG);
        put_u16(&mut self.out, 0);
        put_u16(&mut self.out, 0);
        let n16 = u16::try_from(n).unwrap_or(u16::MAX);
        put_u16(&mut self.out, n16);
        put_u16(&mut self.out, n16);
        put_u32(&mut self.out, u32::try_from(cd_size).unwrap_or(u32::MAX));
        put_u32(&mut self.out, u32::try_from(cd_offset).unwrap_or(u32::MAX));
        put_u16(&mut self.out, 0);
        Ok(())
    }

    /// Una entrada del directorio central, con su extra zip64 si hace falta.
    fn escribe_cd(&mut self, e: &Written) {
        let zip64 = e.comp_size > U32_MAX || e.uncomp_size > U32_MAX || e.offset > U32_MAX;
        let mut extra: Vec<u8> = Vec::new();
        if zip64 {
            // Header 0x0001: los tres campos en 64 bits, en este orden y solo
            // los que en el registro fijo van a 0xFFFFFFFF.
            put_u16(&mut extra, 0x0001);
            put_u16(&mut extra, 24);
            put_u64(&mut extra, e.uncomp_size);
            put_u64(&mut extra, e.comp_size);
            put_u64(&mut extra, e.offset);
        }
        put_u32(&mut self.out, CD_SIG);
        // «Hecho por»: 3 = Unix en el byte alto, para que los permisos del
        // campo de atributos externos signifiquen algo.
        put_u16(&mut self.out, (3 << 8) | VERSION_BASE);
        put_u16(
            &mut self.out,
            if zip64 { VERSION_ZIP64 } else { VERSION_BASE },
        );
        put_u16(&mut self.out, e.flags);
        put_u16(&mut self.out, e.method);
        put_u16(&mut self.out, e.dos_time);
        put_u16(&mut self.out, e.dos_date);
        put_u32(&mut self.out, e.crc);
        put_u32(&mut self.out, trunca(e.comp_size));
        put_u32(&mut self.out, trunca(e.uncomp_size));
        put_u16(
            &mut self.out,
            u16::try_from(e.name.len()).unwrap_or(u16::MAX),
        );
        put_u16(&mut self.out, u16::try_from(extra.len()).unwrap_or(0));
        // Comentario, disco de inicio, atributos internos: los tres a cero.
        // Después van los EXTERNOS (4) y el offset del header local (4), y
        // nada más — un campo de sobra aquí desplaza el nombre y el
        // directorio deja de parsear cuatro bytes más allá.
        put_u16(&mut self.out, 0);
        put_u16(&mut self.out, 0);
        put_u16(&mut self.out, 0);
        put_u32(&mut self.out, e.external_attrs);
        put_u32(&mut self.out, trunca(e.offset));
        self.out.extend_from_slice(&e.name);
        self.out.extend_from_slice(&extra);
    }
}

/// `0xFFFFFFFF` es el centinela que dice «mira el extra de zip64».
fn trunca(v: u64) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// Permisos Unix en el byte alto, más el bit de directorio de MS-DOS.
fn external_attrs(entry: &PackEntry) -> u32 {
    let modo = entry.mode.unwrap_or(if entry.dir { 0o755 } else { 0o644 });
    let tipo = if entry.dir { 0o040_000 } else { 0o100_000 };
    ((tipo | (modo & 0o7777)) << 16) | u32::from(entry.dir)
}

/// Fecha y hora en el formato de MS-DOS, que es lo que zip lleva.
///
/// Antes de 1980 no existe en ese formato: se fija en el 1 de enero de 1980,
/// que es lo que hace todo el mundo. `None` es lo mismo — un cero ahí sería
/// una fecha inválida, no una fecha ausente.
fn dos_datetime(mtime_ms: Option<i64>) -> (u16, u16) {
    const EPOCH_DOS: (u16, u16) = (0, 0b0000_0000_0010_0001);
    let Some(ms) = mtime_ms else {
        return EPOCH_DOS;
    };
    let secs = ms.div_euclid(1000);
    let Some(dt) = civil_from_unix(secs) else {
        return EPOCH_DOS;
    };
    if dt.year < 1980 {
        return EPOCH_DOS;
    }
    let anyo = u16::try_from(dt.year - 1980).unwrap_or(0);
    let date = (anyo << 9) | (u16::from(dt.month) << 5) | u16::from(dt.day);
    let time = (u16::from(dt.hour) << 11) | (u16::from(dt.min) << 5) | u16::from(dt.sec / 2);
    (time, date)
}

/// Fecha civil UTC a partir de segundos desde el epoch.
pub(super) struct Civil {
    pub(super) year: i64,
    pub(super) month: u8,
    pub(super) day: u8,
    pub(super) hour: u8,
    pub(super) min: u8,
    pub(super) sec: u8,
}

/// El algoritmo de Howard Hinnant (`civil_from_days`), sin dependencias: la
/// alternativa era arrastrar `chrono` a un crate de provider por una fecha que
/// solo se escribe.
pub(super) fn civil_from_unix(secs: i64) -> Option<Civil> {
    let dias = secs.div_euclid(86_400);
    let resto = secs.rem_euclid(86_400);
    let z = dias + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    Some(Civil {
        year: if m <= 2 { y + 1 } else { y },
        month: u8::try_from(m).ok()?,
        day: u8::try_from(d).ok()?,
        hour: u8::try_from(resto / 3600).ok()?,
        min: u8::try_from((resto % 3600) / 60).ok()?,
        sec: u8::try_from(resto % 60).ok()?,
    })
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}
