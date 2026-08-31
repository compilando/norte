//! El índice de git (`.git/index`), leído en bytes.
//!
//! Es el fichero que hace barata esta columna: trae, por cada ruta rastreada,
//! el `stat` que git vio la última vez. Comparar eso con el `stat` de ahora
//! contesta «¿cambió?» sin abrir un solo fichero — que es justo lo que un
//! panel puede permitirse hacer por página.
//!
//! Formato: cabecera `DIRC`, versión, número de entradas, y luego las entradas
//! con sus campos en big-endian. Lo que aquí se implementa son las versiones
//! **2 y 3**; la 4 comprime los nombres contra la entrada anterior y se
//! RECHAZA POR SU NOMBRE, porque leerla como si fuese v2 daría rutas
//! inventadas.

extern crate alloc;

use alloc::vec::Vec;

/// Una entrada del índice: la ruta y el `stat` que git guardó.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// Ruta relativa a la raíz del repositorio, en BYTES (regla dura 1).
    pub path: Vec<u8>,
    /// `st_mtime` en segundos, tal como git lo guardó.
    pub mtime_sec: u32,
    /// `st_mtime`, nanosegundos.
    pub mtime_nsec: u32,
    /// `st_ctime` en segundos.
    pub ctime_sec: u32,
    /// `st_ctime`, nanosegundos.
    pub ctime_nsec: u32,
    /// Tamaño que git vio (truncado a 32 bits por el formato).
    pub size: u32,
    /// Inodo, o 0 si git no lo guardó (repositorios de Windows).
    pub ino: u32,
    /// Dispositivo, o 0.
    pub dev: u32,
    /// Modo del fichero.
    pub mode: u32,
    /// Id de objeto del blob que git tiene registrado. Es lo que desempata el
    /// caso «racy», donde el `stat` no dice nada.
    pub oid: [u8; 20],
    /// `true` si la entrada está en un estado de conflicto de merge (stage
    /// distinto de 0). Un fichero en conflicto no es «modificado».
    pub conflicted: bool,
}

/// Por qué un índice no se pudo leer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexError {
    /// No empieza por `DIRC`.
    NotAnIndex,
    /// Versión que este parser no lee. La 4 comprime prefijos de ruta.
    UnsupportedVersion(u32),
    /// Se acabó el fichero a mitad de una entrada.
    Truncated,
}

/// El índice ya parseado. Las entradas quedan en el orden del fichero, que
/// git mantiene ORDENADO por ruta — y eso es lo que hace barato preguntar por
/// un prefijo.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct GitIndex {
    entries: Vec<IndexEntry>,
}

impl GitIndex {
    /// Parsea `.git/index`.
    ///
    /// # Errors
    ///
    /// [`IndexError`] según lo que falle. Las extensiones que van detrás de
    /// las entradas se ignoran: se lee el número de entradas de la cabecera y
    /// se para ahí.
    pub fn parse(raw: &[u8]) -> Result<Self, IndexError> {
        if raw.len() < 12 || &raw[..4] != b"DIRC" {
            return Err(IndexError::NotAnIndex);
        }
        let version = be32(&raw[4..8]);
        if version != 2 && version != 3 {
            return Err(IndexError::UnsupportedVersion(version));
        }
        let count = be32(&raw[8..12]) as usize;
        let mut entries = Vec::with_capacity(count.min(4_096));
        let mut at = 12;
        for _ in 0..count {
            let (entry, next) = parse_entry(raw, at, version)?;
            entries.push(entry);
            at = next;
        }
        Ok(Self { entries })
    }

    /// Cuántas entradas trae.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` si no trae ninguna (un repositorio recién creado).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// La entrada `n`, en el orden del fichero.
    #[must_use]
    pub fn entry(&self, n: usize) -> Option<&IndexEntry> {
        self.entries.get(n)
    }

    /// Las entradas cuya ruta empieza por `prefix`.
    ///
    /// Barato porque el índice está ORDENADO: se busca el primer candidato por
    /// bisección y se sigue mientras el prefijo aguante, en vez de recorrer un
    /// índice de cien mil entradas por cada página de veinte.
    pub fn under_prefix<'a>(&'a self, prefix: &'a [u8]) -> impl Iterator<Item = &'a IndexEntry> {
        let start = self.entries.partition_point(|e| e.path < prefix.to_vec());
        self.entries[start..]
            .iter()
            .take_while(move |e| e.path.starts_with(prefix))
    }

    /// La entrada de una ruta exacta, por bisección.
    #[must_use]
    pub fn get(&self, path: &[u8]) -> Option<&IndexEntry> {
        let at = self.entries.partition_point(|e| e.path.as_slice() < path);
        self.entries.get(at).filter(|e| e.path == path)
    }
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Una entrada v2/v3 y el offset de la siguiente.
///
/// Las entradas van alineadas a 8 bytes con relleno de NULs (v2/v3), así que
/// el avance no es «lo que ocupó el nombre» sino eso redondeado.
fn parse_entry(raw: &[u8], at: usize, version: u32) -> Result<(IndexEntry, usize), IndexError> {
    // 62 bytes fijos + nombre + relleno; v3 mete 2 bytes más de flags.
    const FIXED: usize = 62;
    let extra = usize::from(version >= 3);
    if raw.len() < at + FIXED {
        return Err(IndexError::Truncated);
    }
    let f = &raw[at..];
    let flags = u16::from_be_bytes([f[60], f[61]]);
    // Bit 14 = extended (v3): hay 2 bytes más de flags antes del nombre.
    let extended = usize::from(extra == 1 && flags & 0x4000 != 0) * 2;
    // Los 12 bits bajos son el largo del nombre, con 0xFFF = "más largo que
    // eso, busca el NUL".
    let name_len = usize::from(flags & 0x0FFF);
    // Bits 12-13 = stage: distinto de 0 es un conflicto de merge.
    let conflicted = (flags >> 12) & 0x3 != 0;
    let name_at = at + FIXED + extended;
    if raw.len() < name_at {
        return Err(IndexError::Truncated);
    }
    let name_end = if name_len == 0x0FFF {
        name_at
            + raw[name_at..]
                .iter()
                .position(|b| *b == 0)
                .ok_or(IndexError::Truncated)?
    } else {
        let end = name_at + name_len;
        if raw.len() < end {
            return Err(IndexError::Truncated);
        }
        end
    };
    let entry = IndexEntry {
        path: raw[name_at..name_end].to_vec(),
        ctime_sec: be32(&f[0..4]),
        ctime_nsec: be32(&f[4..8]),
        mtime_sec: be32(&f[8..12]),
        mtime_nsec: be32(&f[12..16]),
        dev: be32(&f[16..20]),
        ino: be32(&f[20..24]),
        mode: be32(&f[24..28]),
        size: be32(&f[36..40]),
        oid: {
            let mut oid = [0u8; 20];
            oid.copy_from_slice(&f[40..60]);
            oid
        },
        conflicted,
    };
    // Relleno hasta múltiplo de 8, contando desde el principio de la entrada.
    let used = name_end - at;
    let padded = used + (8 - used % 8);
    Ok((entry, at + padded))
}

/// La forja de índices que usan los tests de ESTE módulo y los de `status`.
/// Vive fuera de `mod tests` para que otro módulo pueda usarla sin duplicar
/// el formato — que es justo lo que haría que las dos copias divergieran.
#[cfg(test)]
pub mod tests_support {
    use super::*;

    /// Un índice v2 con `(ruta, tamaño, mtime, oid)` por entrada.
    #[must_use]
    pub fn forja(entradas: &[(&[u8], u32, u32, [u8; 20])]) -> Vec<u8> {
        let con_modo: Vec<_> = entradas
            .iter()
            .map(|(n, s, m, o)| (*n, *s, *m, *o, 0o100_644u32))
            .collect();
        forja_con_modo(&con_modo)
    }

    /// [`forja`] con el MODO de cada entrada, que es lo único que distingue un
    /// submódulo (`0o160000`, el «gitlink») de un fichero normal.
    #[must_use]
    pub fn forja_con_modo(entradas: &[(&[u8], u32, u32, [u8; 20], u32)]) -> Vec<u8> {
        let mut out = b"DIRC".to_vec();
        out.extend_from_slice(&2u32.to_be_bytes());
        out.extend_from_slice(&(entradas.len() as u32).to_be_bytes());
        for (name, size, mtime, oid, mode) in entradas {
            let start = out.len();
            out.extend_from_slice(&7u32.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&mtime.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&3u32.to_be_bytes());
            out.extend_from_slice(&5u32.to_be_bytes());
            out.extend_from_slice(&mode.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&size.to_be_bytes());
            out.extend_from_slice(oid);
            let len = u16::try_from(name.len()).unwrap_or(0x0FFF).min(0x0FFF);
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(name);
            let used = out.len() - start;
            out.extend(core::iter::repeat_n(0u8, 8 - used % 8));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Forja un índice v2 con las entradas dadas: cabecera `DIRC`, versión y
    /// cuenta, y cada entrada con sus 62 bytes fijos, el nombre y el relleno.
    fn index_v2_con(entradas: &[(&[u8], u32)]) -> Vec<u8> {
        forja(2, entradas, 0)
    }

    fn forja(version: u32, entradas: &[(&[u8], u32)], stage: u16) -> Vec<u8> {
        let mut out = b"DIRC".to_vec();
        out.extend_from_slice(&version.to_be_bytes());
        out.extend_from_slice(&(entradas.len() as u32).to_be_bytes());
        for (name, size) in entradas {
            let start = out.len();
            out.extend_from_slice(&7u32.to_be_bytes()); // ctime sec
            out.extend_from_slice(&0u32.to_be_bytes()); // ctime nsec
            out.extend_from_slice(&11u32.to_be_bytes()); // mtime sec
            out.extend_from_slice(&0u32.to_be_bytes()); // mtime nsec
            out.extend_from_slice(&3u32.to_be_bytes()); // dev
            out.extend_from_slice(&5u32.to_be_bytes()); // ino
            out.extend_from_slice(&0o100_644u32.to_be_bytes()); // mode
            out.extend_from_slice(&0u32.to_be_bytes()); // uid
            out.extend_from_slice(&0u32.to_be_bytes()); // gid
            out.extend_from_slice(&size.to_be_bytes()); // size
            out.extend_from_slice(&[0u8; 20]); // sha1
            let len = u16::try_from(name.len()).unwrap_or(0x0FFF).min(0x0FFF);
            out.extend_from_slice(&(len | (stage << 12)).to_be_bytes());
            out.extend_from_slice(name);
            let used = out.len() - start;
            out.extend(std::iter::repeat_n(0u8, 8 - used % 8));
        }
        out
    }

    #[test]
    fn parsea_v2_y_conserva_los_bytes_del_nombre() {
        let raw = index_v2_con(&[(b"cp437-\xa4\xa5.txt", 3), (b"src/lib.rs", 10)]);
        let idx = GitIndex::parse(&raw).unwrap();
        assert_eq!(idx.len(), 2);
        assert_eq!(idx.entry(0).unwrap().path, b"cp437-\xa4\xa5.txt");
        assert_eq!(idx.entry(1).unwrap().size, 10);
        assert_eq!(idx.entry(1).unwrap().mtime_sec, 11);
        assert_eq!(idx.entry(1).unwrap().ino, 5);
    }

    #[test]
    fn la_version_4_se_rechaza_por_su_nombre() {
        let mut raw = index_v2_con(&[(b"a", 1)]);
        raw[7] = 4;
        assert_eq!(
            GitIndex::parse(&raw),
            Err(IndexError::UnsupportedVersion(4)),
            "v4 comprime prefijos de ruta; decirlo es mejor que leer basura"
        );
    }

    #[test]
    fn algo_que_no_es_un_indice_se_dice() {
        assert_eq!(GitIndex::parse(b"nope"), Err(IndexError::NotAnIndex));
        assert_eq!(GitIndex::parse(&[]), Err(IndexError::NotAnIndex));
    }

    #[test]
    fn un_indice_truncado_no_inventa_entradas() {
        let raw = index_v2_con(&[(b"a.txt", 1), (b"b.txt", 1)]);
        assert_eq!(
            GitIndex::parse(&raw[..raw.len() - 10]),
            Err(IndexError::Truncated)
        );
    }

    #[test]
    fn el_indice_esta_ordenado_y_eso_es_lo_que_hace_barato_el_prefijo() {
        let raw = index_v2_con(&[(b"a/b.txt", 1), (b"a/c.txt", 1), (b"z.txt", 1)]);
        let idx = GitIndex::parse(&raw).unwrap();
        assert_eq!(idx.under_prefix(b"a/").count(), 2);
        assert_eq!(idx.under_prefix(b"").count(), 3);
        assert_eq!(idx.under_prefix(b"q").count(), 0);
    }

    #[test]
    fn get_encuentra_una_ruta_exacta_y_no_su_prefijo() {
        let raw = index_v2_con(&[(b"a/b.txt", 1), (b"ab.txt", 2)]);
        let idx = GitIndex::parse(&raw).unwrap();
        assert_eq!(idx.get(b"ab.txt").unwrap().size, 2);
        assert!(idx.get(b"a/").is_none(), "un prefijo no es una entrada");
    }

    /// Stage distinto de 0 = conflicto de merge. Un fichero en conflicto no es
    /// «modificado», y llamarlo así escondería lo que de verdad pasa.
    #[test]
    fn una_entrada_en_conflicto_se_marca() {
        let raw = forja(2, &[(b"peleado.txt", 1)], 2);
        let idx = GitIndex::parse(&raw).unwrap();
        assert!(idx.entry(0).unwrap().conflicted);
    }
}
