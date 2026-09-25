//! How a field enters a digest, for whoever produces a hash of the protocol
//! (ADR 0051, #174).
//!
//! Two primitives and one encoder, and all three exist so there is never two
//! versions of them:
//!
//! * [`feed`] prepends the LENGTH, so `"ab"+"c"` and `"a"+"bc"` can never give
//!   the same digest. Without that, two adjacent fields read as one.
//! * [`feed_opt`] additionally prepends a PRESENCE byte, so "there is no
//!   field" and "empty field" do not collide either — without it both would
//!   be `len=0`.
//! * [`hex_lower`] is the form a digest takes on the wire. Always lowercase:
//!   a second encoder is a second chance to write uppercase, which is the
//!   detail that makes two writes of the same hash compare unequal.
//!
//! # Why it lives HERE and not in `norte-core`
//!
//! `norte-core` has its own copy and **it does not move** (ADR 0051, option
//! B). That is not an oversight or a leftover: that copy is the journal's
//! tamper-evident chain (ADR 0023) and the anchor of the audit export
//! (ADR 0025), and changing one byte of its framing invalidates
//! `verify_chain` on EVERY `journal.db` already on disk. That is a migration,
//! not a refactor.
//!
//! What WAS closed off is the possibility of the two silently DRIFTING:
//! `norte-core` has a test that feeds both implementations the same
//! inputs — including the hostile corpus — and compares the bytes. A change
//! here that drifts from there does not compile a release: it breaks that
//! test.
//!
//! `norte-sync` has no copy: it uses this one. Its natural dependency was
//! upward (`norte-core` depends on `norte-sync`, not the other way around),
//! so sharing through `norte-core` was not an option, and the place that does
//! see everything that speaks the protocol is this crate — the same one that
//! already had
//! [`PlanHash::from_digest`](crate::methods::PlanHash::from_digest), which now
//! calls [`hex_lower`] instead of carrying its own copy of the loop.

use sha2::{Digest, Sha256};

/// Feeds a field with its LENGTH in front (`u64` little-endian), so that two
/// adjacent fields can never be read as a single one.
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
/// assert_ne!(a.finalize(), b.finalize(), "the length prefix keeps them apart");
/// ```
pub fn feed(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

/// OPTIONAL field, with a presence byte (`0` = absent, `1` = present) in
/// front of the framed field.
///
/// ```
/// use norte_proto::hashing::feed_opt;
/// use sha2::{Digest, Sha256};
///
/// let mut absent = Sha256::new();
/// feed_opt(&mut absent, None);
/// let mut empty = Sha256::new();
/// feed_opt(&mut empty, Some(b""));
/// assert_ne!(
///     absent.finalize(),
///     empty.finalize(),
///     "\"no field\" is not \"empty field\""
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

/// LOWERCASE hex: the form a digest takes on the wire.
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

    /// FROZEN VECTOR, the twin of the one `norte-core` has over its own copy.
    /// The relative tests (`assert_ne!` between two digests) would still pass
    /// even if the prefix changed from `u64` to `u32` or from little to
    /// big-endian, and either of those two things breaks the `plan_hash` of
    /// every plan already emitted.
    #[test]
    fn the_framing_is_a_frozen_vector() {
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

    /// Bytes that are not UTF-8 pass through as is: the framing is of BYTES
    /// (hard rule 1), and a hostile name has to hash the same here as in the
    /// journal.
    #[test]
    fn non_utf8_bytes_are_left_untouched() {
        let hostile = digest(|h| feed(h, b"caf\xff.txt"));
        let other = digest(|h| feed(h, b"caf\xfe.txt"));
        assert_ne!(hostile, other);
    }
}
