//! Can the destination hold its own writes? (#164, ADR 0054)
//!
//! A recursive `Copy` composes `destination + relative` step by step, and a
//! symlink placed on an INTERMEDIATE component between the human saying yes
//! and the bytes being written sends the copy somewhere else. Where the
//! system knows how to open relative to a descriptor — Linux and macOS — the
//! core opens the root once and that detour stops existing. Where it does
//! not — Windows, SFTP, a bucket — the copy is done the same way it always
//! has been, by path.
//!
//! What happens here is a WARNING about that second case, not a refusal. The
//! same contract as [`crate::space`]: put the fact in front and let the human
//! decide. Refusing would leave unable to copy exactly the destinations that
//! cannot offer that defense, which is a far higher price than the race it
//! avoids — and that race requires someone with access to the destination
//! tree to plant a symlink at the exact moment.
//!
//! # What the line does NOT promise
//!
//! Its absence says the destination KNOWS how to confine. Since #219 that
//! reaches EVERY transfer, not only recursive ones: a lone leaf also hangs
//! off an approved tree — its destination directory, the one the human chose
//! — and the core opens its root. The reasoning that said otherwise ("a leaf
//! has no window to exploit") was false: between the gate and the bytes there
//! is the collision `stat`, the staging creation, its publish and up to three
//! retries, each one resolving the path again.
//!
//! What remains uncovered, and why this does not promise "this operation is
//! confined" but "this place knows how to confine": an INTERMEDIATE component
//! of the approved directory swapped out before it is opened. The anchor is
//! obtained by opening a path, so that first resolution is by path by
//! definition — it is the same residue a recursive copy accepts for its own
//! destination. What the capability describes is the LOCATION, which is what
//! ADR 0054 is about.

use norte_i18n::{Lang, t_in};
use norte_proto::{Capabilities, CapabilityFlags};

/// The warning, or `None` when the destination knows how to confine.
///
/// Knowing how is NOT announced: a line on every copy is noise, and noise
/// teaches people to skip the line on the exact day it says something.
///
/// ```
/// use norte_frontend::confine::warning;
/// use norte_i18n::Lang;
/// use norte_proto::{Capabilities, CapabilityFlags};
///
/// let confines = Capabilities {
///     flags: CapabilityFlags::CONFINED_WRITES,
///     max_path: None,
/// };
/// assert!(warning(confines, Lang::En).is_none());
///
/// let cannot = Capabilities { flags: CapabilityFlags::empty(), max_path: None };
/// assert!(warning(cannot, Lang::En).is_some(), "the human decides, but informed");
/// ```
#[must_use]
pub fn warning(caps: Capabilities, lang: Lang) -> Option<String> {
    if caps.flags.contains(CapabilityFlags::CONFINED_WRITES) {
        return None;
    }
    Some(t_in(lang, "confine-warning"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(flags: CapabilityFlags) -> Capabilities {
        Capabilities {
            flags,
            max_path: None,
        }
    }

    /// A destination that confines says nothing.
    #[test]
    fn a_confining_destination_stays_quiet() {
        assert_eq!(
            warning(caps(CapabilityFlags::CONFINED_WRITES), Lang::En),
            None
        );
    }

    /// And one that cannot, says so — without blocking anything.
    #[test]
    fn a_destination_that_cannot_confine_warns() {
        let warning_text = warning(caps(CapabilityFlags::empty()), Lang::En)
            .expect("the human decides, but informed");
        assert!(!warning_text.is_empty(), "the key exists in the catalogue");
    }

    /// The rest of the flags have no say in this: what is checked is ONE, and
    /// a destination loaded with capabilities that do not include this one
    /// still warns.
    #[test]
    fn no_other_flag_silences_it() {
        let noisy = CapabilityFlags::all() - CapabilityFlags::CONFINED_WRITES;
        assert!(warning(caps(noisy), Lang::En).is_some());
    }

    /// And the line exists in both languages: a missing key would come out as
    /// the key's name, which is worse than not warning at all.
    #[test]
    fn the_line_exists_in_both_languages() {
        let en = warning(caps(CapabilityFlags::empty()), Lang::En).expect("en");
        let es = warning(caps(CapabilityFlags::empty()), Lang::Es).expect("es");
        assert_ne!(en, "confine-warning", "untranslated key in English");
        assert_ne!(es, "confine-warning", "untranslated key in Spanish");
        assert_ne!(en, es, "and they are not the same sentence");
    }
}
