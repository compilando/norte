//! Cómo un campo entra en un digest, para todo el que produzca un hash del
//! protocolo (ADR 0051, #174).
//!
//! Dos primitivas y un codificador, y las tres existen para que no haya dos
//! versiones de ellas:
//!
//! * [`feed`] antepone la LONGITUD, así que `"ab"+"c"` y `"a"+"bc"` no pueden
//!   dar el mismo digest. Sin eso, dos campos adyacentes se leen como uno.
//! * [`feed_opt`] antepone además un byte de PRESENCIA, así que «no hay campo»
//!   y «campo vacío» tampoco colisionan — sin él los dos serían `len=0`.
//! * [`hex_lower`] es la forma en la que un digest sale al wire. En minúscula
//!   siempre: un segundo codificador es una segunda ocasión de escribir
//!   mayúsculas, que es el detalle por el que dos escrituras del mismo hash
//!   comparan distinto.
//!
//! # Por qué vive AQUÍ y no en `norte-core`
//!
//! `norte-core` tiene su propia copia y **no se mueve** (ADR 0051, opción B).
//! No es descuido ni pendiente: esa copia es la cadena tamper-evident del
//! journal (ADR 0023) y el ancla de la exportación de auditoría (ADR 0025), y
//! cambiar un byte de su framing invalida `verify_chain` en TODOS los
//! `journal.db` que ya están en disco. Eso es una migración, no un refactor.
//!
//! Lo que sí se cerró es la posibilidad de que las dos DERIVEN en silencio:
//! `norte-core` tiene un test que alimenta las dos implementaciones con las
//! mismas entradas —incluido el corpus hostil— y compara los bytes. Un cambio
//! aquí que se aparte de allí no compila un release: rompe ese test.
//!
//! `norte-sync` no tiene copia: usa ésta. Su dependencia natural era hacia
//! arriba (`norte-core` depende de `norte-sync`, no al revés), así que
//! compartir por `norte-core` no era una opción, y el sitio que sí ve todo el
//! que habla el protocolo es este crate — el mismo que ya tenía
//! [`PlanHash::from_digest`](crate::methods::PlanHash::from_digest), que ahora
//! llama a [`hex_lower`] en vez de llevar su propia copia del bucle.

use sha2::{Digest, Sha256};

/// Alimenta un campo con su LONGITUD delante (`u64` little-endian), de forma
/// que dos campos adyacentes jamás se lean como uno solo.
///
/// ```
/// use norte_proto::hashing::feed;
/// use sha2::{Digest, Sha256};
///
/// let mut a = Sha256::new();
/// feed(&mut a, b"ab");
/// feed(&mut a, b"c");
/// let mut b = Sha256::new();
/// feed(&mut b, b"a");
/// feed(&mut b, b"bc");
/// assert_ne!(a.finalize(), b.finalize(), "el prefijo de longitud los separa");
/// ```
pub fn feed(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

/// Campo OPCIONAL, con byte de presencia (`0` = ausente, `1` = presente)
/// delante del campo enmarcado.
///
/// ```
/// use norte_proto::hashing::feed_opt;
/// use sha2::{Digest, Sha256};
///
/// let mut ausente = Sha256::new();
/// feed_opt(&mut ausente, None);
/// let mut vacio = Sha256::new();
/// feed_opt(&mut vacio, Some(b""));
/// assert_ne!(
///     ausente.finalize(),
///     vacio.finalize(),
///     "«no hay campo» no es «campo vacío»"
/// );
/// ```
pub fn feed_opt(digest: &mut Sha256, value: Option<&[u8]>) {
    match value {
        None => digest.update([0u8]),
        Some(bytes) => {
            digest.update([1u8]);
            feed(digest, bytes);
        }
    }
}

/// Hex MINÚSCULA: la forma en la que un digest sale al wire.
///
/// ```
/// use norte_proto::hashing::hex_lower;
/// assert_eq!(hex_lower(&[0xab, 0x0f]), "ab0f");
/// assert_eq!(hex_lower(&[]), "");
/// ```
#[must_use]
pub fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(f: impl FnOnce(&mut Sha256)) -> [u8; 32] {
        let mut h = Sha256::new();
        f(&mut h);
        h.finalize().into()
    }

    /// VECTOR CONGELADO, el gemelo del que `norte-core` tiene sobre su copia.
    /// Los tests relativos (`assert_ne!` entre dos digests) pasarían igual si
    /// el prefijo cambiase de `u64` a `u32` o de little a big-endian, y
    /// cualquiera de esas dos cosas rompe el `plan_hash` de todo plan ya
    /// emitido.
    #[test]
    fn el_framing_es_un_vector_congelado() {
        let d = digest(|h| {
            feed(h, b"ab");
            feed_opt(h, None);
            feed_opt(h, Some(b"c"));
        });
        assert_eq!(
            hex_lower(&d),
            "34cdea21137e823d6af96d7f45c48b02dfd1b592f62fd8edeaf70c3fc8e4feba"
        );
    }

    /// Bytes que no son UTF-8 pasan tal cual: el framing es de BYTES (regla
    /// dura 1), y un nombre hostil tiene que hashear igual aquí que en el
    /// journal.
    #[test]
    fn los_bytes_no_utf8_no_se_tocan() {
        let hostil = digest(|h| feed(h, b"caf\xff.txt"));
        let otro = digest(|h| feed(h, b"caf\xfe.txt"));
        assert_ne!(hostil, otro);
    }
}
