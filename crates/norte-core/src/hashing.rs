//! Cómo el core mete campos en un digest, en UN sitio.
//!
//! El framing con longitud lo usan la cadena de hashes del journal (ADR 0023)
//! y el `plan_hash` del batch de renames, y por el mismo motivo: dos campos
//! adyacentes JAMÁS se pueden leer como uno solo. El hex minúscula lo usan esos
//! dos más la exportación de auditoría (ADR 0025). Tenerlo dos veces es tenerlo
//! dos veces mal en cuanto uno de los dos cambie: el framing de un hash tiene
//! que ser idéntico para todos los que lo usan o deja de significar lo mismo.
//!
//! **`feed`/`feed_opt` son el framing de una cadena tamper-evident sobre una
//! base de datos que ya existe en disco.** Cambiar un byte aquí invalida
//! `verify_chain` en todos los journals ya escritos. Salieron de `journal.rs`
//! sin tocar una línea y así tienen que seguir.
//!
//! # #174: esta copia se queda, y ya no puede derivar en silencio
//! El framing vive también en [`norte_proto::hashing`], que es donde ADR 0051
//! decidió ponerlo: `norte-sync` lo usa desde allí y ya no tiene copia
//! propia. **Ésta no se mueve.** Es la cadena tamper-evident del journal
//! (ADR 0023) y el ancla de la exportación de auditoría (ADR 0025), así que su
//! framing no puede cambiar ni un byte sin invalidar todo `journal.db` ya
//! escrito — eso es una migración, no un refactor. Y relicenciarla tampoco es
//! gratis: este crate es AGPL-3.0-only y `norte-proto` es MIT OR Apache-2.0.
//!
//! Lo que la duplicación tenía de peligroso —que las dos derivaran sin que
//! nadie lo notara, que es exactamente lo que le pasó a la clave de plegado de
//! #151— lo cierra `tests::el_framing_de_proto_es_byte_a_byte_este`: alimenta
//! las dos implementaciones con las mismas entradas, incluido el corpus
//! hostil, y compara los digests. Dos copias que no pueden discrepar en
//! silencio son un coste de mantenimiento; dos que sí pueden son un bug
//! esperando.

use sha2::{Digest, Sha256};

/// Alimenta un campo con su LONGITUD delante, así `"ab"+"c"` y `"a"+"bc"` no
/// producen el mismo digest.
pub(crate) fn feed(h: &mut Sha256, bytes: &[u8]) {
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

/// Campo opcional con BYTE DE PRESENCIA (0/1) → `None` y `Some(vacío)` NUNCA
/// colisionan (sin él, ambos serían `len=0` — hallazgo security B1).
pub(crate) fn feed_opt(h: &mut Sha256, o: Option<&[u8]>) {
    match o {
        None => h.update([0u8]),
        Some(b) => {
            h.update([1u8]);
            feed(h, b);
        }
    }
}

/// Hex MINÚSCULA, la forma en la que un digest sale del core (el `plan_hash`
/// del wire, el head del journal, el ancla de auditoría).
pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(HEX[usize::from(b >> 4)]));
        s.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(f: impl FnOnce(&mut Sha256)) -> [u8; 32] {
        let mut h = Sha256::new();
        f(&mut h);
        h.finalize().into()
    }

    /// #174 / ADR 0051: las dos implementaciones del framing producen los
    /// MISMOS bytes. Es lo que sustituye a «extraer y borrar la copia», que
    /// aquí no se puede hacer sin tocar el formato del journal.
    ///
    /// El corpus hostil entra a propósito: si alguna de las dos tratara los
    /// bytes como texto —lossy, normalización, lo que sea— sería justo ahí
    /// donde se vería, y no con `b"ab"`.
    #[test]
    fn el_framing_de_proto_es_byte_a_byte_este() {
        fn proto(f: impl FnOnce(&mut Sha256)) -> [u8; 32] {
            let mut h = Sha256::new();
            f(&mut h);
            h.finalize().into()
        }

        assert_eq!(
            digest(|h| feed(h, b"ab")),
            proto(|h| norte_proto::hashing::feed(h, b"ab")),
        );
        assert_eq!(
            digest(|h| feed_opt(h, None)),
            proto(|h| norte_proto::hashing::feed_opt(h, None)),
        );
        assert_eq!(
            digest(|h| feed_opt(h, Some(b""))),
            proto(|h| norte_proto::hashing::feed_opt(h, Some(b""))),
        );
        for name in norte_testkit::corpus::hostile_names() {
            assert_eq!(
                digest(|h| {
                    feed(h, &name.bytes);
                    feed_opt(h, Some(&name.bytes));
                }),
                proto(|h| {
                    norte_proto::hashing::feed(h, &name.bytes);
                    norte_proto::hashing::feed_opt(h, Some(&name.bytes));
                }),
                "el framing difiere sobre {}: {}",
                name.id,
                name.why
            );
        }
        // Y el hex, que es la otra mitad que no puede tener dos formas.
        assert_eq!(
            hex_lower(&[0xab, 0x0f]),
            norte_proto::hashing::hex_lower(&[0xab, 0x0f])
        );
    }

    /// El prefijo de longitud es lo único que separa dos campos pegados.
    #[test]
    fn length_prefix_prevents_concatenation_collision() {
        let ab_c = digest(|h| {
            feed(h, b"ab");
            feed(h, b"c");
        });
        let a_bc = digest(|h| {
            feed(h, b"a");
            feed(h, b"bc");
        });
        assert_ne!(ab_c, a_bc);
    }

    /// Sin byte de presencia, «no hay campo» y «campo vacío» serían el mismo
    /// digest, y un `Option` dejaría de ser tamper-evident.
    #[test]
    fn absent_and_empty_are_different_digests() {
        assert_ne!(
            digest(|h| feed_opt(h, None)),
            digest(|h| feed_opt(h, Some(b""))),
        );
    }

    /// VECTOR CONGELADO del framing. Los otros dos tests son RELATIVOS
    /// (`assert_ne!` entre dos digests), así que pasarían igual si el prefijo
    /// de longitud cambiase de `u64` a `u32`, de little-endian a big-endian, o
    /// si el byte de presencia intercambiase 0 y 1 — y cualquiera de esas tres
    /// invalida `verify_chain` en TODOS los journals que ya están en disco.
    ///
    /// Si este test se pone rojo, has roto la cadena de todos ellos. No
    /// actualices la constante: revierte el cambio, o versiona el formato y
    /// migra.
    #[test]
    fn the_framing_is_frozen() {
        let d = digest(|h| {
            feed(h, b"ab");
            feed_opt(h, None);
            feed_opt(h, Some(b""));
            feed_opt(h, Some(b"\xff\xfe"));
        });
        assert_eq!(
            hex_lower(&d),
            "5e1f0d8206deef60ba905cf913d6d83046e1bf877103b88514a7f93a373c9fbe",
        );
    }

    #[test]
    fn hex_lower_is_two_lowercase_digits_per_byte() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xff, 0xa5]), "000fffa5");
        assert_eq!(hex_lower(&[]), "");
    }
}
