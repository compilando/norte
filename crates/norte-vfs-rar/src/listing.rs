//! Parse de lo que IMPRIMIÓ el delegado. Puro sobre bytes: no lanza procesos,
//! así que las reglas de encoding se prueban en una máquina sin `7z` ni
//! `unrar` instalados.
//!
//! Los nombres son [`Vec<u8>`] y jamás `String` (regla 1): que los bytes hayan
//! llegado por una tubería no los vuelve UTF-8.

/// Una entrada tal y como la imprimió el delegado, aún SIN validar como
/// segmento de `VPath` — de eso se encarga el índice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEntry {
    /// Bytes crudos del nombre, tal cual salieron de la tubería.
    pub name: Vec<u8>,
    /// Tamaño descomprimido en bytes.
    pub size: u64,
    /// La entrada es un directorio.
    pub is_dir: bool,
    /// mtime en segundos epoch, si el delegado lo imprimió y era legible.
    pub mtime: Option<i64>,
    /// La entrada está cifrada (leerla pediría contraseña; el runner NUNCA
    /// deja que se pida).
    pub encrypted: bool,
    /// La entrada forma parte de un bloque sólido.
    pub solid: bool,
}

/// El resultado de un parse: entradas legibles y **cuántas se saltaron**.
///
/// Contar las omitidas es el mismo contrato de «omitir CON señal» de ADR 0018:
/// un archivo con una entrada rara se explora igual, pero el usuario se entera.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// Las entradas que se pudieron leer.
    pub entries: Vec<RawEntry>,
    /// Cuántos registros se descartaron por no ser interpretables.
    pub skipped: u64,
}

/// Un registro en construcción: pares clave/valor en bytes.
#[derive(Default)]
struct Record<'a> {
    fields: Vec<(&'a [u8], &'a [u8])>,
    malformed: bool,
}

impl<'a> Record<'a> {
    fn is_empty(&self) -> bool {
        self.fields.is_empty() && !self.malformed
    }

    fn get(&self, key: &[u8]) -> Option<&'a [u8]> {
        self.fields
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| *v)
    }
}

/// Parte una línea en `clave<sep>valor`, con el separador ya elegido por el
/// formato. La clave se recorta por ambos lados; el valor solo por la
/// izquierda **un** espacio y por la derecha un `\r`: un nombre puede acabar
/// en espacio y perderlo sería mostrar un fichero que no es ese fichero.
fn split_field<'a>(line: &'a [u8], sep: &[u8]) -> Option<(&'a [u8], &'a [u8])> {
    let (key, value) = match line.windows(sep.len()).position(|w| w == sep) {
        Some(at) => (&line[..at], &line[at + sep.len()..]),
        // `Created =` sin valor: 7z imprime la clave y nada detrás.
        None => (line.strip_suffix(trim_ascii(sep))?, &line[line.len()..]),
    };
    let key = trim_ascii(key);
    // Las claves reales llevan espacios (`Packed Size`, `Host OS`, `NT
    // Security`): exigir una sola palabra descartaba media salida de 7z. Lo
    // que sí se exige es que empiece por letra y no traiga nada que un
    // nombre de fichero continuado sí traería.
    if !key.first().is_some_and(u8::is_ascii_alphabetic)
        || key
            .iter()
            .any(|b| !(b.is_ascii_alphanumeric() || *b == b' '))
    {
        return None;
    }
    Some((key, value))
}

fn trim_ascii(mut s: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = s {
        if first.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = s {
        if last.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    s
}

/// Recorre `stdout` por líneas agrupando registros separados por línea en
/// blanco, y llama a `emit` con cada uno. Un registro con alguna línea que no
/// parsea como campo se marca `malformed`.
fn for_each_record<'a>(stdout: &'a [u8], sep: &[u8], mut emit: impl FnMut(&Record<'a>)) {
    let mut current = Record::default();
    for raw in stdout.split(|b| *b == b'\n') {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        if trim_ascii(line).is_empty() {
            if !current.is_empty() {
                emit(&current);
            }
            current = Record::default();
            continue;
        }
        match split_field(line, sep) {
            Some((k, v)) => current.fields.push((k, v)),
            None => current.malformed = true,
        }
    }
    if !current.is_empty() {
        emit(&current);
    }
}

fn parse_u64(v: &[u8]) -> u64 {
    std::str::from_utf8(trim_ascii(v))
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// `YYYY-MM-DD HH:MM:SS[,fraction]` (hora local del delegado, que es lo único
/// que imprime) a segundos epoch. Devuelve `None` ante cualquier desviación:
/// un mtime inventado es peor que ninguno.
fn parse_timestamp(value: &[u8]) -> Option<i64> {
    let text = std::str::from_utf8(trim_ascii(value)).ok()?;
    let (date, time) = text.split(',').next()?.split_once(' ')?;
    let mut fields = date.split('-');
    let (year, month, day): (i64, i64, i64) = (
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
    );
    if fields.next().is_some() {
        return None;
    }
    let mut fields = time.split(':');
    let (hour, minute, second): (i64, i64, i64) = (
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
    );
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Días desde 1970-01-01 (algoritmo `days_from_civil` de Hinnant, dominio
/// público). Se implementa aquí para no arrastrar una dependencia de fechas
/// entera por un `Modified =` de un listado.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Parsea la salida de `7z l -slt -- <archivo>`.
///
/// El bloque de cabecera describe el ARCHIVO (su primer `Path =` es el `.rar`,
/// no una entrada) y termina en la línea de guiones; todo lo anterior se
/// ignora sin contarlo como omitido.
///
/// ```
/// let out = norte_vfs_rar::parse_7z_slt(b"----------\nPath = a.txt\nSize = 3\n");
/// assert_eq!(out.entries[0].name, b"a.txt");
/// ```
#[must_use]
pub fn parse_7z_slt(stdout: &[u8]) -> Listing {
    // Sin la línea de guiones no hay listado que leer: 7z no llegó a empezar.
    let Some(body) = stdout
        .windows(10)
        .position(|w| w == b"----------")
        .map(|at| &stdout[at + 10..])
    else {
        return Listing::default();
    };
    let mut out = Listing::default();
    for_each_record(body, b" = ", |rec| {
        let Some(name) = rec.get(b"Path").filter(|_| !rec.malformed) else {
            out.skipped += 1;
            return;
        };
        let attrs = rec.get(b"Attributes").unwrap_or_default();
        out.entries.push(RawEntry {
            name: name.to_vec(),
            size: rec.get(b"Size").map_or(0, parse_u64),
            is_dir: rec.get(b"Folder").is_some_and(|v| trim_ascii(v) == b"+")
                || attrs.starts_with(b"D"),
            mtime: rec.get(b"Modified").and_then(parse_timestamp),
            encrypted: rec.get(b"Encrypted").is_some_and(|v| trim_ascii(v) == b"+"),
            solid: rec.get(b"Solid").is_some_and(|v| trim_ascii(v) != b"-"),
        });
    });
    out
}

/// Parsea la salida de `unrar vt -- <archivo>`.
///
/// Un registro SIN `Name:` es la cabecera del archivo (`Archive:`,
/// `Details:`), no una entrada perdida: se ignora sin contarlo.
///
/// ```
/// let out = norte_vfs_rar::parse_unrar_vt(b"\n        Name: a.txt\n        Type: File\n");
/// assert_eq!(out.entries[0].name, b"a.txt");
/// ```
#[must_use]
pub fn parse_unrar_vt(stdout: &[u8]) -> Listing {
    let mut out = Listing::default();
    for_each_record(stdout, b":", |rec| {
        // Sin `Name:` el registro es la cabecera (el banner de unrar, el
        // `Archive:`/`Details:`), no una entrada perdida: ignorar sin contar.
        let Some(name) = rec.get(b"Name") else {
            return;
        };
        if rec.malformed {
            out.skipped += 1;
            return;
        }
        let flags = rec.get(b"Flags").unwrap_or_default();
        out.entries.push(RawEntry {
            name: trim_leading_space(name).to_vec(),
            size: rec.get(b"Size").map_or(0, parse_u64),
            is_dir: rec
                .get(b"Type")
                .is_some_and(|v| trim_ascii(v).eq_ignore_ascii_case(b"Directory")),
            mtime: rec.get(b"mtime").and_then(parse_timestamp),
            encrypted: contains(flags, b"encrypted"),
            solid: contains(flags, b"solid"),
        });
    });
    out
}

/// Quita UN espacio inicial (el del `key: value`), no los que el nombre
/// pudiera llevar de verdad.
fn trim_leading_space(v: &[u8]) -> &[u8] {
    v.strip_prefix(b" ").unwrap_or(v)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Salida real de `7z l -slt` (7-Zip 26.02) recortada a dos entradas.
    const SEVENZ_SLT: &[u8] = b"\
Listing archive: t.rar\n\
\n\
--\n\
Path = t.rar\n\
Type = Rar5\n\
Solid = -\n\
\n\
----------\n\
Path = hello.txt\n\
Folder = -\n\
Size = 11\n\
Modified = 2021-01-14 10:25:36\n\
Encrypted = -\n\
Solid = -\n\
CRC = DB187CE4\n\
\n\
Path = cp437-\xa4\xa5.txt\n\
Folder = -\n\
Size = 16\n\
Modified = 2021-01-14 10:25:36\n\
Encrypted = -\n\
Solid = -\n\
";

    #[test]
    fn slt_ignora_la_cabecera_del_archivo_y_conserva_bytes_crudos() {
        let out = parse_7z_slt(SEVENZ_SLT);
        // `Path = t.rar` es el archivo mismo, no una entrada: va antes del
        // `----------`.
        assert_eq!(out.entries.len(), 2, "la cabecera no es una entrada");
        assert_eq!(out.entries[0].name, b"hello.txt");
        assert_eq!(out.entries[0].size, 11);
        assert_eq!(
            out.entries[1].name, b"cp437-\xa4\xa5.txt",
            "bytes crudos, no lossy"
        );
        assert_eq!(out.skipped, 0);
    }

    #[test]
    fn slt_lee_directorio_cifrado_y_solido() {
        let raw = b"----------\nPath = d\nFolder = +\nSize = 0\nEncrypted = +\nSolid = +\n";
        let out = parse_7z_slt(raw);
        let e = &out.entries[0];
        assert!(e.is_dir && e.encrypted && e.solid);
    }

    #[test]
    fn slt_convierte_modified_a_epoch() {
        let raw = b"----------\nPath = a\nSize = 0\nModified = 1970-01-02 00:00:01\n";
        assert_eq!(parse_7z_slt(raw).entries[0].mtime, Some(86_401));
    }

    /// Salida real de `unrar vt` (UNRAR 7.23). El nombre no-UTF8 llega
    /// TRUNCADO por el propio unrar: `cp437-` sin extensión. No es un bug del
    /// parser, y es por lo que 7z va primero.
    const UNRAR_VT: &[u8] = b"\
\n\
Archive: t.rar\n\
Details: RAR 5\n\
\n\
        Name: hello.txt\n\
        Type: File\n\
        Size: 11\n\
       mtime: 2021-01-14 09:25:36,000000000\n\
  Attributes: ----r-----\n\
\n\
        Name: dir/nested.txt\n\
        Type: Directory\n\
        Size: 0\n\
       mtime: 2021-01-14 09:25:36,000000000\n\
  Attributes: ----r-----\n\
";

    #[test]
    fn vt_lee_nombre_tipo_y_tamano() {
        let out = parse_unrar_vt(UNRAR_VT);
        assert_eq!(out.entries.len(), 2);
        assert_eq!(out.entries[0].name, b"hello.txt");
        assert!(!out.entries[0].is_dir);
        assert!(out.entries[1].is_dir, "Type: Directory");
        assert_eq!(out.skipped, 0, "la cabecera del archivo no es una omitida");
    }

    #[test]
    fn vt_no_recorta_un_espacio_final_del_nombre() {
        let out = parse_unrar_vt(b"\n        Name: raro \n        Type: File\n");
        assert_eq!(out.entries[0].name, b"raro ", "el nombre acaba en espacio");
    }

    /// Regresión medida contra 7-Zip 26.02: media salida real lleva claves
    /// CON ESPACIOS (`Packed Size`, `Host OS`, `NT Security`) y claves con
    /// valor VACÍO (`Created =`). Exigir una clave de una sola palabra
    /// marcaba cada registro como ilegible y el listado salía vacío.
    #[test]
    fn slt_claves_con_espacios_y_valor_vacio_no_rompen_el_registro() {
        let raw = b"----------\nPath = a.txt\nFolder = -\nSize = 8\nPacked Size = 8\n\
Created = \nAccessed =\nHost OS = Unix\nNT Security = \n";
        let out = parse_7z_slt(raw);
        assert_eq!(out.skipped, 0, "ninguna de esas líneas es ilegible");
        assert_eq!(out.entries.len(), 1);
        assert_eq!(out.entries[0].size, 8);
    }

    /// El banner de unrar (`UNRAR 7.23 freeware  Copyright (c) …`) no parsea
    /// como campo y NO es una entrada perdida: se ignora sin contarla.
    #[test]
    fn vt_el_banner_no_cuenta_como_omitida() {
        let raw = b"\nUNRAR 7.23 freeware      Copyright (c) 1993-2026 Alexander Roshal\n\
\nArchive: t.rar\nDetails: RAR 5\n\n        Name: a.txt\n        Type: File\n";
        let out = parse_unrar_vt(raw);
        assert_eq!(out.entries.len(), 1);
        assert_eq!(out.skipped, 0);
    }

    #[test]
    fn un_nombre_con_salto_de_linea_se_salta_y_se_cuenta() {
        // Una salida por líneas no puede llevar un `\n` dentro de un nombre
        // sin adivinar. Adivinar aquí es enseñar un fichero que no es ese
        // fichero.
        let raw = b"----------\nPath = ok.txt\nSize = 1\n\nPath = mal\nnombre.txt\nSize = 2\n";
        let out = parse_7z_slt(raw);
        assert_eq!(out.entries.len(), 1);
        assert_eq!(out.skipped, 1, "saltada y CONTADA, como ADR 0018");
    }

    #[test]
    fn vt_un_registro_ilegible_se_salta_y_se_cuenta() {
        let raw = b"\n        Name: mal\nnombre.txt\n        Type: File\n";
        let out = parse_unrar_vt(raw);
        assert!(out.entries.is_empty());
        assert_eq!(out.skipped, 1);
    }

    #[test]
    fn sin_separador_el_listado_esta_vacio_y_no_cuenta_omitidas() {
        let out = parse_7z_slt(b"ERROR: cannot open t.rar\n");
        assert_eq!(out, Listing::default());
    }
}
