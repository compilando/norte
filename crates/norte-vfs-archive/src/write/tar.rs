//! Escritor de tar INCREMENTAL, con el mismo contrato que el de zip.
//!
//! Formato ustar con la extensión GNU para nombres largos (typeflag `L`): un
//! nombre de más de 100 bytes no se recorta —recortar un nombre es perderlo—,
//! viaja en una entrada propia delante de la suya.
//!
//! Escrito a mano y no con `tar::Builder` por lo mismo que el índice de zip lo
//! está: el `Builder` quiere un `Read` por entrada, y aquí los bytes llegan a
//! trozos desde un provider asíncrono.

use super::{PackEntry, PackError};

/// Un bloque de tar. Todo, cabeceras incluidas, es múltiplo de esto.
const BLOQUE: usize = 512;
/// Lo que cabe en el campo `name` de una cabecera ustar.
const NAME_MAX: usize = 100;
/// Typeflag de la entrada GNU que lleva un nombre largo.
const TYPE_LONGNAME: u8 = b'L';
/// Typeflag de fichero regular.
const TYPE_FILE: u8 = b'0';
/// Typeflag de directorio.
const TYPE_DIR: u8 = b'5';

/// Escritor incremental de tar.
pub struct TarWriter {
    out: Vec<u8>,
    curso: Option<u64>,
    /// Bytes escritos de la entrada en curso, para la cola de relleno.
    escritos: u64,
    cerrado: bool,
}

impl TarWriter {
    /// Un escritor vacío.
    #[must_use]
    pub fn new() -> Self {
        Self {
            out: Vec::new(),
            curso: None,
            escritos: 0,
            cerrado: false,
        }
    }

    /// Bytes producidos hasta ahora; vacía el búfer.
    pub fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    /// Abre una entrada. `entry.size` tiene que ser el tamaño EXACTO que se va
    /// a escribir: tar lo lleva en la cabecera, delante de los datos.
    ///
    /// # Errors
    ///
    /// [`PackError::Estado`] si ya hay una entrada abierta o el archivo está
    /// cerrado.
    pub fn begin(&mut self, entry: &PackEntry) -> Result<(), PackError> {
        if self.curso.is_some() || self.cerrado {
            return Err(PackError::Estado);
        }
        // El tamaño tiene que caber en los once dígitos octales de ustar: por
        // encima, la cabecera mentiría y el archivo sería ilegible a partir de
        // esa entrada. Se dice; no se escribe (ver [`TAR_SIZE_MAX`]).
        if !entry.dir && entry.size > TAR_SIZE_MAX {
            return Err(PackError::Tamano);
        }
        let mut name = entry.name.clone();
        if entry.dir && !name.ends_with(b"/") {
            name.push(b'/');
        }
        if name.len() > NAME_MAX {
            // GNU longname: una entrada `L` cuyo CONTENIDO es el nombre, y
            // detrás la de verdad con el nombre recortado (que los lectores
            // que entienden `L` ignoran, y los que no, al menos ven algo).
            let mut cabecera = [0_u8; BLOQUE];
            escribe_cabecera(
                &mut cabecera,
                b"././@LongLink",
                name.len() as u64,
                TYPE_LONGNAME,
                0o644,
                None,
            );
            self.out.extend_from_slice(&cabecera);
            self.out.extend_from_slice(&name);
            self.rellena(name.len() as u64);
        }
        let corto: Vec<u8> = name.iter().copied().take(NAME_MAX).collect();
        let size = if entry.dir { 0 } else { entry.size };
        let mut cabecera = [0_u8; BLOQUE];
        escribe_cabecera(
            &mut cabecera,
            &corto,
            size,
            if entry.dir { TYPE_DIR } else { TYPE_FILE },
            entry.mode.unwrap_or(if entry.dir { 0o755 } else { 0o644 }),
            entry.mtime_ms,
        );
        self.out.extend_from_slice(&cabecera);
        self.curso = Some(size);
        self.escritos = 0;
        Ok(())
    }

    /// Añade datos a la entrada abierta.
    ///
    /// # Errors
    ///
    /// [`PackError::Estado`] sin entrada abierta, o si el trozo pasa del
    /// tamaño anunciado en la cabecera — un tar cuya cabecera miente es un tar
    /// que nadie puede leer más allá de esa entrada.
    pub fn data(&mut self, chunk: &[u8]) -> Result<(), PackError> {
        let size = self.curso.ok_or(PackError::Estado)?;
        let nuevos = self.escritos.saturating_add(chunk.len() as u64);
        if nuevos > size {
            return Err(PackError::Tamano);
        }
        self.escritos = nuevos;
        self.out.extend_from_slice(chunk);
        Ok(())
    }

    /// Cierra la entrada abierta y la rellena hasta el bloque.
    ///
    /// # Errors
    ///
    /// [`PackError::Estado`] sin entrada abierta, [`PackError::Tamano`] si se
    /// escribieron menos bytes de los anunciados.
    pub fn end(&mut self) -> Result<(), PackError> {
        let size = self.curso.take().ok_or(PackError::Estado)?;
        if self.escritos != size {
            return Err(PackError::Tamano);
        }
        self.rellena(size);
        Ok(())
    }

    /// Escribe la marca de fin: dos bloques a cero.
    ///
    /// # Errors
    ///
    /// [`PackError::Estado`] si queda una entrada abierta.
    pub fn finish(&mut self) -> Result<(), PackError> {
        if self.curso.is_some() {
            return Err(PackError::Estado);
        }
        self.out.extend_from_slice(&[0_u8; BLOQUE * 2]);
        self.cerrado = true;
        Ok(())
    }

    /// Ceros hasta cerrar el bloque de 512.
    fn rellena(&mut self, escrito: u64) {
        // El resto de dividir por 512 cabe en `usize` en cualquier
        // arquitectura: es menor que 512.
        let resto = usize::try_from(escrito % BLOQUE as u64).unwrap_or(0);
        if resto != 0 {
            self.out.extend(std::iter::repeat_n(0_u8, BLOQUE - resto));
        }
    }
}

impl Default for TarWriter {
    fn default() -> Self {
        Self::new()
    }
}

/// Rellena una cabecera ustar y calcula su checksum.
fn escribe_cabecera(
    h: &mut [u8; BLOQUE],
    name: &[u8],
    size: u64,
    typeflag: u8,
    mode: u32,
    mtime_ms: Option<i64>,
) {
    let n = name.len().min(NAME_MAX);
    h[..n].copy_from_slice(&name[..n]);
    octal(&mut h[100..108], u64::from(mode & 0o7777), 7);
    octal(&mut h[108..116], 0, 7); // uid
    octal(&mut h[116..124], 0, 7); // gid
    octal(&mut h[124..136], size, 11);
    let mtime = mtime_ms.map_or(0, |ms| ms.div_euclid(1000).max(0));
    octal(&mut h[136..148], u64::try_from(mtime).unwrap_or(0), 11);
    // El checksum se calcula con su propio campo lleno de espacios.
    h[148..156].fill(b' ');
    h[156] = typeflag;
    h[257..262].copy_from_slice(b"ustar");
    h[263..265].copy_from_slice(b"00");
    let suma: u32 = h.iter().map(|b| u32::from(*b)).sum();
    octal(&mut h[148..155], u64::from(suma), 6);
    h[155] = b' ';
}

/// Lo más grande que cabe en el campo de tamaño de ustar: once dígitos
/// octales, o sea 8 GiB menos uno.
///
/// No es una curiosidad del formato: `octal` se quedaba con los dígitos BAJOS,
/// así que un fichero de exactamente 8 GiB se anunciaba como de tamaño CERO y
/// detrás iban sus ocho gigas. El relleno se calculaba del tamaño real, así
/// que el flujo seguía alineado y la corrupción no se veía hasta que un lector
/// intentaba parsear esos bytes como cabeceras. Un archivo así lo escribía la
/// task entera sin un solo error, y el journal anotaba un `Created`.
pub(super) const TAR_SIZE_MAX: u64 = 0o777_7777_7777;

/// Un número en octal ASCII, alineado a la derecha con ceros y terminado en
/// NUL, que es como tar los lleva.
///
/// El valor tiene que CABER: quien llama comprueba antes
/// ([`TAR_SIZE_MAX`]). Aquí, si no cupiera, se rellena de nueves octales —
/// un valor visiblemente absurdo— en vez de quedarse con los dígitos bajos,
/// que es lo que convertía un desbordamiento en un tamaño plausible y falso.
fn octal(campo: &mut [u8], v: u64, digitos: usize) {
    let s = format!("{v:0>digitos$o}");
    let bytes = s.as_bytes();
    if bytes.len() > digitos {
        campo[..digitos].fill(b'7');
    } else {
        let n = bytes.len().min(digitos);
        campo[digitos - n..digitos].copy_from_slice(bytes);
    }
    if digitos < campo.len() {
        campo[digitos] = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Un tamaño que no cabe en once dígitos octales se DICE.**
    ///
    /// `octal` se quedaba con los dígitos bajos, así que 8 GiB exactos se
    /// escribían como tamaño CERO con ocho gigas detrás: el relleno salía del
    /// tamaño real, el flujo seguía alineado, y la corrupción no aparecía
    /// hasta que un lector intentaba parsear esos bytes como cabeceras. La
    /// task terminaba en verde y el journal anotaba un `Created`.
    #[test]
    fn un_tamano_que_no_cabe_en_ustar_se_rehusa() {
        let mut w = TarWriter::new();
        assert_eq!(
            w.begin(&PackEntry::file(b"vm.img".to_vec(), TAR_SIZE_MAX + 1)),
            Err(PackError::Tamano)
        );
        // Y justo debajo del tope sí entra.
        assert!(
            w.begin(&PackEntry::file(b"vm.img".to_vec(), TAR_SIZE_MAX))
                .is_ok()
        );
    }

    /// El campo octal es de dígitos ALTOS: si algo no cupiera, se ve que no
    /// cupo en vez de parecer un número plausible.
    #[test]
    fn el_octal_no_se_queda_con_los_digitos_bajos() {
        let mut campo = [0_u8; 12];
        octal(&mut campo, 8, 11);
        assert_eq!(&campo[..11], b"00000000010", "ocho es 10 en octal");
        // Un valor que se pasa: nueves, no ceros.
        octal(&mut campo, TAR_SIZE_MAX + 1, 11);
        assert_eq!(&campo[..11], b"77777777777");
    }

    /// La cabecera de un nombre largo NO recorta el nombre: viaja entero en su
    /// entrada `L`, y el que se queda en los 100 bytes es una copia.
    #[test]
    fn un_nombre_largo_viaja_entero_en_su_entrada_propia() {
        let largo = vec![b'a'; 150];
        let mut w = TarWriter::new();
        w.begin(&PackEntry::file(largo.clone(), 0)).expect("abre");
        let bytes = w.take();
        assert_eq!(&bytes[..13], b"././@LongLink", "la entrada GNU va delante");
        assert_eq!(bytes[156], TYPE_LONGNAME);
        assert_eq!(&bytes[BLOQUE..BLOQUE + largo.len()], &largo[..], "entero");
        // Y detrás, la cabecera de verdad con los 100 primeros bytes.
        let real = BLOQUE * 2;
        assert_eq!(&bytes[real..real + 100], &largo[..100]);
    }
}
