//! A password half-typed, shared by both frontends.
//!
//! It lives here and not in a frontend by rule D14, and this time with the
//! strongest reason of all: it is a SECURITY type. Two implementations of
//! "what has been typed in a password field" are two places where the
//! redacting `Debug` gets forgotten, two places where the buffer is not
//! zeroed on drop, and — worse — a frontend inheriting the version with
//! neither, because it was born later. The TUI had it since #325; the window
//! needs it for #327, and what is shared is the GUARANTEE, not the
//! resemblance.

use zeroize::Zeroizing;

/// Cap on the characters of a typed password.
///
/// The same as the TUI's text fields (`TEXT_FIELD_MAX_CHARS`), and here it is
/// also structural: it fixes the capacity reserved up front, so going past it
/// means reallocating — which is exactly what leaves pieces of the password
/// unzeroed on the heap. See "Why it reserves space up front".
pub const SECRET_MAX_CHARS: usize = 256;

/// The BYTES that must be reserved for [`SECRET_MAX_CHARS`] characters.
///
/// Four per character, the maximum for UTF-8. And this is not a courtesy
/// margin: the reservation used to be made with the number of CHARACTERS,
/// i.e. 256 bytes, while the cap was applied in characters. A password of 150
/// accented letters is 300 bytes and fewer than 256 characters — so it fit
/// under the cap and did NOT fit in the reservation, so `String` reallocated,
/// copied, and freed the old block **without zeroing it**: half the password
/// stayed on the heap. Exactly what the type promises does not happen.
const RESERVED_BYTES: usize = SECRET_MAX_CHARS * 4;

/// What has been typed into a password field.
///
/// It exists for two things a `String` does not give, and neither is
/// optional (rule 10):
///
/// * **A `Debug` that REDACTS.** The modals derive `Debug`, and that `Debug`
///   ends up in `tracing`, in a panic message and in an `assert_eq!`'s diff.
///   `Zeroizing<String>` delegates its `Debug` to the `String`, so without
///   this wrapper the password would print in all three places.
/// * **Wiped on drop.** The inside is `Zeroizing`: the buffer is overwritten
///   with zeros on drop, instead of staying on the heap for a core dump or
///   swap.
///
/// # Why it reserves space up front
///
/// `Zeroizing` wipes the ENTIRE current allocation, capacity included — and
/// only that one: its own documentation says it "cannot ensure that previous
/// reallocations did not leave values on the heap". A `String` that grows
/// 4→8→16→… leaves unzeroed pieces of the half-typed password along the way.
/// Being born with the BYTES for [`SECRET_MAX_CHARS`] characters already
/// reserved — four per character; see `RESERVED_BYTES` for why counting
/// characters there was a bug — there is no reallocation at all, and
/// zeroize's "best effort" becomes exact for this copy. The copies beyond
/// [`TypedSecret::expose`] (the params, the serialized frame, the daemon's
/// `Value`) are still not wiped: see ADR 0015, which says which ones are and
/// which are not.
///
/// `PartialEq` is derived for the tests (comparing two modals) and compares
/// in NON-constant time: never pass it a value from an untrusted source.
///
/// ```
/// use norte_frontend::secret::TypedSecret;
/// let mut s = TypedSecret::default();
/// assert!(s.is_empty());
/// s.push('h');
/// s.push('i');
/// assert_eq!(s.chars(), 2);
/// // What is painted are DOTS, not the text.
/// assert_eq!(s.dots(), "••");
/// // And the `Debug` does not say it.
/// assert_eq!(format!("{s:?}"), "TypedSecret(***)");
/// ```
#[derive(PartialEq, Eq)]
pub struct TypedSecret(Zeroizing<String>);

impl Clone for TypedSecret {
    /// By hand, and not derived, so the copy is BORN with the reservation.
    ///
    /// `String::clone` allocates capacity equal to the length, so a derived
    /// clone starts exactly full: the first character appended to it would
    /// make it reallocate, and that is where a piece of the password is left
    /// unzeroed. Today nobody writes into a clone — the TUI clones the modal
    /// to read it — but the trap was armed and it costs three lines to
    /// disarm it.
    fn clone(&self) -> Self {
        let mut copy = Self::default();
        copy.0.push_str(&self.0);
        copy
    }
}

impl Default for TypedSecret {
    fn default() -> Self {
        // See "Why it reserves space up front": `String::new()` here would
        // reintroduce reallocations and, with them, the leftovers on the
        // heap.
        Self(Zeroizing::new(String::with_capacity(RESERVED_BYTES)))
    }
}

impl std::fmt::Debug for TypedSecret {
    /// Never the content. Not the length either: it is information about the
    /// password, and knowing whether anything has been typed is enough for
    /// debugging.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_empty() {
            "TypedSecret(empty)"
        } else {
            "TypedSecret(***)"
        })
    }
}

impl TypedSecret {
    /// The plaintext, to hand over via `connection.provide_secret`.
    ///
    /// Calling it is saying "the secret really is needed here" — do not use
    /// it to paint or to log.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// How many characters have been typed, to paint the dots.
    #[must_use]
    pub fn chars(&self) -> usize {
        self.0.chars().count()
    }

    /// Is it empty? Confirming over an empty field hands nothing over.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// What gets PAINTED: one dot per character.
    ///
    /// A method and not a decision each renderer makes, because the
    /// alternative is one of the two painting the text. There is no way to
    /// get this wrong: the only thing that comes out of this type for the
    /// paint layer is dots.
    #[must_use]
    pub fn dots(&self) -> String {
        "•".repeat(self.chars())
    }

    /// Appends a typed character, up to [`SECRET_MAX_CHARS`].
    ///
    /// The cap stops a stuck key from growing the buffer past what is
    /// reserved — which is when `String` reallocates and leaves a piece of
    /// the password unzeroed on the heap. Stopping silently is what the other
    /// text fields do: the field looks full.
    pub fn push(&mut self, c: char) {
        if self.0.chars().count() >= SECRET_MAX_CHARS {
            return;
        }
        self.0.push(c);
    }

    /// Deletes the last character (backspace).
    pub fn pop(&mut self) {
        self.0.pop();
    }

    /// Replaces what was typed with `text`, respecting the cap.
    ///
    /// Exists for the window, where the field is edited by the webview and
    /// the WHOLE text arrives after every keystroke instead of one character:
    /// the caret belongs to the renderer, and rebuilding it in Rust would
    /// mean keeping two ideas of where the cursor is. Whatever is left over
    /// the cap is simply discarded — the same as [`Self::push`] stopping
    /// silently.
    ///
    /// The previous buffer is zeroed BEFORE writing the new one: without
    /// that, typing six characters would leave five prefixes of the password
    /// intact on the heap, which is exactly what this type exists to
    /// prevent.
    pub fn set(&mut self, text: &str) {
        use zeroize::Zeroize;
        // `Zeroize for String` overwrites the written bytes, does `clear()`
        // and ALSO overwrites the free capacity — but does not free it: the
        // allocation survives. That is exactly what is needed, because it
        // means every prefix ever typed ends up overwritten without
        // reallocating anything. The `reserve` below is defense in depth: on
        // a buffer that already has the reservation it does nothing.
        self.0.zeroize();
        self.0.reserve(RESERVED_BYTES);
        for c in text.chars().take(SECRET_MAX_CHARS) {
            self.0.push(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `Debug` says neither the content nor the length.
    ///
    /// The length is not innocent either: it is information about the
    /// password, and this `Debug` ends up in `tracing`, in a panic and in the
    /// diff of an `assert_eq!` over the whole modal.
    #[test]
    fn debug_redacts() {
        let mut s = TypedSecret::default();
        assert_eq!(format!("{s:?}"), "TypedSecret(empty)");
        s.set("correcthorsebatterystaple");
        let d = format!("{s:?}");
        assert_eq!(d, "TypedSecret(***)");
        assert!(!d.contains("horse"), "the content does not come out: {d}");
        assert!(!d.contains("25"), "the length does not either: {d}");
    }

    /// The only thing shown for painting is dots, one per CHARACTER.
    ///
    /// Per character and not per byte: with bytes, a password with accents
    /// would paint longer than it is, which leaks its composition.
    #[test]
    fn only_dots_come_out_one_per_character() {
        let mut s = TypedSecret::default();
        s.set("cañón€");
        assert_eq!(s.chars(), 6);
        assert_eq!(s.dots(), "••••••");
        assert_eq!(s.dots().chars().count(), s.chars());
    }

    /// The cap stops silently, and `set` does not bypass it.
    ///
    /// Going past the cap means reallocating, and reallocating means leaving
    /// a piece of the password unzeroed on the heap — the whole reason the
    /// reservation exists.
    #[test]
    fn the_cap_stops_typing_and_replacing_alike() {
        let mut s = TypedSecret::default();
        for _ in 0..(SECRET_MAX_CHARS + 50) {
            s.push('x');
        }
        assert_eq!(s.chars(), SECRET_MAX_CHARS);

        let long = "y".repeat(SECRET_MAX_CHARS + 50);
        s.set(&long);
        assert_eq!(s.chars(), SECRET_MAX_CHARS);
        assert!(s.expose().chars().all(|c| c == 'y'), "replaced whole");
    }

    /// The reservation is measured in BYTES, not characters, and a password
    /// full of accents does not reallocate.
    ///
    /// This was a real bug: the reservation was made with the number of
    /// characters (256 bytes) and the cap was applied in characters, so 150
    /// accented letters — 300 bytes — fit under the cap and not in the
    /// reservation. `String` reallocated, copied, and freed the old block
    /// WITHOUT zeroing it: half the password on the heap, exactly what this
    /// type promises does not happen.
    ///
    /// Checked by CAPACITY and not by the pointer, because what has to be
    /// asserted is that it never needed to grow.
    #[test]
    fn a_multibyte_password_does_not_reallocate() {
        let mut s = TypedSecret::default();
        let cap = s.0.capacity();
        assert!(
            cap >= SECRET_MAX_CHARS * 4,
            "the reservation is in bytes: {cap}"
        );
        // The cap's worst case: 256 characters of four bytes each.
        let worst: String = std::iter::repeat_n('\u{1F600}', SECRET_MAX_CHARS).collect();
        s.set(&worst);
        assert_eq!(s.chars(), SECRET_MAX_CHARS);
        assert_eq!(
            s.0.capacity(),
            cap,
            "it grew: there was a reallocation, and the old block was freed unzeroed"
        );

        // And via `push`, which is the TUI's path.
        let mut t = TypedSecret::default();
        let cap = t.0.capacity();
        for _ in 0..SECRET_MAX_CHARS {
            t.push('ñ');
        }
        assert_eq!(
            t.0.capacity(),
            cap,
            "the other path does not reallocate either"
        );
    }

    /// A clone is born WITH the reservation, not full.
    ///
    /// `String::clone` allocates capacity equal to the length, so the derived
    /// clone used to reallocate on the first character appended to it.
    #[test]
    fn a_clone_is_born_with_its_reservation() {
        let mut s = TypedSecret::default();
        s.set("something");
        let c = s.clone();
        assert_eq!(c.expose(), "something");
        assert!(
            c.0.capacity() >= SECRET_MAX_CHARS * 4,
            "the clone was born full: {}",
            c.0.capacity()
        );
    }

    /// Clearing and typing again works, and `set("")` leaves the field inert.
    #[test]
    fn clearing_leaves_the_field_inert() {
        let mut s = TypedSecret::default();
        s.set("something");
        assert!(!s.is_empty());
        s.set("");
        assert!(s.is_empty(), "confirming over this hands nothing over");
        assert_eq!(s.dots(), "");
        s.push('a');
        assert_eq!(s.chars(), 1, "still usable after clearing it");
    }

    /// Backspace removes ONE character, not one byte.
    #[test]
    fn backspace_removes_one_character() {
        let mut s = TypedSecret::default();
        s.set("añ");
        s.pop();
        assert_eq!(s.expose(), "a");
    }
}
