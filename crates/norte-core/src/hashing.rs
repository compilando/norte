//! How the core feeds fields into a digest, in ONE place.
//!
//! The length-prefixed framing is used by the journal's hash chain (ADR 0023)
//! and the batch rename's `plan_hash`, and for the same reason: two adjacent
//! fields must NEVER be readable as one. The lowercase hex is used by those
//! two plus the audit export (ADR 0025). Having it twice is having it twice
//! wrong as soon as one of the two changes: a hash's framing has to be
//! identical for everyone who uses it, or it stops meaning the same thing.
//!
//! **`feed`/`feed_opt` are the framing of a tamper-evident chain over a
//! database that already exists on disk.** Changing a byte here invalidates
//! `verify_chain` on every journal already written. They came out of
//! `journal.rs` without touching a line, and they have to stay that way.
//!
//! # #174: this copy stays, and can no longer drift silently
//! The framing also lives in [`norte_proto::hashing`], which is where ADR
//! 0051 decided to put it: `norte-sync` uses it from there and no longer has
//! its own copy. **This one does not move.** It is the journal's
//! tamper-evident chain (ADR 0023) and the audit export's anchor (ADR 0025),
//! so its framing cannot change by even one byte without invalidating every
//! `journal.db` already written — that is a migration, not a refactor. And
//! relicensing it is not free either: this crate is AGPL-3.0-only and
//! `norte-proto` is MIT OR Apache-2.0.
//!
//! What was dangerous about the duplication —that the two could drift
//! without anyone noticing, which is exactly what happened to #151's folding
//! key— is closed by `tests::the_proto_framing_is_byte_for_byte_this_one`:
//! it feeds both implementations the same inputs, including the hostile
//! corpus, and compares the digests. Two copies that cannot silently
//! disagree are a maintenance cost; two that can are a bug waiting to
//! happen.

use sha2::{Digest, Sha256};

/// Feeds a field with its LENGTH in front, so `"ab"+"c"` and `"a"+"bc"` do
/// not produce the same digest.
pub(crate) fn feed(h: &mut Sha256, bytes: &[u8]) {
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

/// Optional field with a PRESENCE BYTE (0/1) → `None` and `Some(empty)` NEVER
/// collide (without it, both would be `len=0` — security finding B1).
pub(crate) fn feed_opt(h: &mut Sha256, o: Option<&[u8]>) {
    match o {
        None => h.update([0u8]),
        Some(b) => {
            h.update([1u8]);
            feed(h, b);
        }
    }
}

/// LOWERCASE hex, the form a digest takes coming out of the core (the wire's
/// `plan_hash`, the journal head, the audit anchor).
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

    /// #174 / ADR 0051: the two framing implementations produce the SAME
    /// bytes. This is what stands in for "extract and delete the copy",
    /// which cannot be done here without touching the journal format.
    ///
    /// The hostile corpus is included on purpose: if either implementation
    /// treated the bytes as text —lossy, normalization, whatever— this is
    /// exactly where it would show, and not with `b"ab"`.
    #[test]
    fn the_proto_framing_is_byte_for_byte_this_one() {
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
                "framing differs on {}: {}",
                name.id,
                name.why
            );
        }
        // And the hex, the other half that cannot have two forms.
        assert_eq!(
            hex_lower(&[0xab, 0x0f]),
            norte_proto::hashing::hex_lower(&[0xab, 0x0f])
        );
    }

    /// The length prefix is the only thing separating two adjacent fields.
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

    /// Without a presence byte, "no field" and "empty field" would be the
    /// same digest, and an `Option` would stop being tamper-evident.
    #[test]
    fn absent_and_empty_are_different_digests() {
        assert_ne!(
            digest(|h| feed_opt(h, None)),
            digest(|h| feed_opt(h, Some(b""))),
        );
    }

    /// FROZEN VECTOR for the framing. The other two tests are RELATIVE
    /// (`assert_ne!` between two digests), so they would still pass if the
    /// length prefix changed from `u64` to `u32`, from little-endian to
    /// big-endian, or if the presence byte swapped 0 and 1 — and any of
    /// those three invalidates `verify_chain` on EVERY journal already on
    /// disk.
    ///
    /// If this test goes red, you have broken the chain for all of them. Do
    /// not update the constant: revert the change, or version the format
    /// and migrate.
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
