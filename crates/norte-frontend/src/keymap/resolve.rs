//! Resolution state for ONE in-flight sequence. Owns its effective keymap:
//! hot-reload (ADR 0007) builds a new one and swaps the resolver whole.

use super::chord::{Chord, KeyCode, Mods};
use super::effective::{Availability, Effective, Lookup};

/// The ceiling on a typed count, in digits. `9999` repetitions of a cursor
/// move on a listing of any realistic size lands on the last row; a fifth
/// digit is DROPPED rather than wrapping the accumulator into a number the
/// user did not type.
const MAX_COUNT_DIGITS: u32 = 4;

/// The largest count the accumulator will hold, derived from
/// [`MAX_COUNT_DIGITS`] so the two can never disagree.
const MAX_COUNT: u32 = 10u32.pow(MAX_COUNT_DIGITS) - 1;

/// What a typed count did to the command it landed on.
///
/// The count rides WITH the command and the FRONTEND repeats the dispatch, so
/// no command's signature changes and no command can forget to honour one.
///
/// ```
/// use norte_frontend::keymap::{Count, Effective, Resolution, Resolver, Screen, parse_chord, parse_keymap};
///
/// let preset = parse_keymap(
///     "counts = true\n[pane]\nkeymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n",
/// )
/// .unwrap();
/// let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
/// let mut r = Resolver::new(eff);
/// assert_eq!(r.push(parse_chord("5").unwrap()), Resolution::Counting(5));
/// assert_eq!(
///     r.push(parse_chord("j").unwrap()),
///     Resolution::Run { command: "cursor.down".to_owned(), count: Count::Repeat(5) },
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Count {
    /// No count was typed.
    None,
    /// Run the command this many times.
    Repeat(u32),
    /// A count was typed and this command does not take one (the catalogue's
    /// `counts` field says so). Run it ONCE and tell the user the count was
    /// ignored — never swallow it.
    Ignored(u32),
}

/// The digit a bare chord spells, if it spells one. A digit with any modifier
/// is an ordinary chord: `ctrl+5` was never a count.
///
/// `pub(super)` because the LOAD-time digit rule (K2a, `effective.rs`) shares
/// it: two definitions of "is this a digit" is exactly how the count rule and
/// the load rule would drift apart.
pub(super) fn digit_of(chord: Chord) -> Option<u32> {
    match chord.parts() {
        (mods, KeyCode::Char(c)) if mods == Mods::default() => c.to_digit(10),
        _ => None,
    }
}

impl Count {
    /// How many times the frontend runs the dispatch. **Never zero** — an
    /// `Ignored` count runs the command once, exactly like no count at all,
    /// and `Repeat(0)` cannot be typed (zero never opens a count) but is
    /// clamped anyway rather than silently dropping the keystroke.
    ///
    /// The policy lives HERE, not in each frontend: three call sites (the
    /// TUI's key arm, the GUI's dual pane, the GUI's viewer) had a private
    /// copy of the same `match`, which is how the three of them come to
    /// disagree.
    ///
    /// ```
    /// use norte_frontend::keymap::Count;
    ///
    /// assert_eq!(Count::None.times(), 1);
    /// assert_eq!(Count::Repeat(5).times(), 5);
    /// // A count the command does not take runs it ONCE — never zero times,
    /// // and never five.
    /// assert_eq!(Count::Ignored(5).times(), 1);
    /// ```
    #[must_use]
    pub fn times(self) -> u32 {
        match self {
            Self::Repeat(n) => n.max(1),
            Self::None | Self::Ignored(_) => 1,
        }
    }
}

/// Result of pushing a key into the [`Resolver`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Complete sequence: run this command, `count` times.
    Run {
        /// The command to run.
        command: String,
        /// What the typed count means for it, if there was one.
        count: Count,
    },
    /// Valid prefix of some sequence: waiting (current depth).
    Pending(usize),
    /// A count is being typed (current value). The status bar paints it: a
    /// count that cannot be seen is a count that cannot be cancelled.
    Counting(u32),
    /// The key IS bound, and what it is bound to cannot run here. The
    /// frontend says so; it never just does nothing.
    Unavailable {
        /// The command the key is bound to.
        command: String,
        /// Why it cannot run.
        why: Availability,
    },
    /// No binding (or cancellation): clean state, key discarded.
    Reset,
}

/// Resolution state for ONE sequence in progress. OWNS its effective
/// keymap: hot-reload (ADR 0007) builds a new one and replaces the whole
/// resolver.
#[derive(Debug, Clone)]
pub struct Resolver {
    eff: Effective,
    pending: Vec<Chord>,
    /// The count typed so far, if the preset enables counts.
    count: Option<u32>,
}

impl Resolver {
    /// Clean resolver over an effective keymap.
    #[must_use]
    pub fn new(eff: Effective) -> Self {
        Self {
            eff,
            pending: Vec::new(),
            count: None,
        }
    }

    /// The pending sequence (to paint it in the status bar).
    #[must_use]
    pub fn pending(&self) -> &[Chord] {
        &self.pending
    }

    /// The count typed so far (for the status bar). `None` when no digit is
    /// in flight.
    ///
    /// ```
    /// use norte_frontend::keymap::{Effective, Resolver, Screen, parse_chord, parse_keymap};
    ///
    /// let preset = parse_keymap(
    ///     "counts = true\n[pane]\nkeymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n",
    /// )
    /// .unwrap();
    /// let eff = Effective::build_for(&preset, &[], &["cursor.down"], Screen::Browse).unwrap();
    /// let mut r = Resolver::new(eff);
    /// assert_eq!(r.count(), None);
    /// r.push(parse_chord("1").unwrap());
    /// r.push(parse_chord("2").unwrap());
    /// assert_eq!(r.count(), Some(12));
    /// // And it is consumed with the command: it does not survive the
    /// // keystroke.
    /// r.push(parse_chord("j").unwrap());
    /// assert_eq!(r.count(), None);
    /// ```
    #[must_use]
    pub fn count(&self) -> Option<u32> {
        self.count
    }

    /// The effective keymap this resolver owns (G3c): the GUI needs it to
    /// build the command palette's rows (`palette::first_chord`) without
    /// duplicating the `Effective` in a separate `NorteGui` field — the
    /// resolver is already the sole source of truth for the current keymap
    /// (hot-reload replaces it whole, see the type's doc).
    #[must_use]
    pub fn effective(&self) -> &Effective {
        &self.eff
    }

    /// Breaks any pending sequence AND the count in progress (a key the
    /// frontend does not model is equivalent to a miss: it cancels the
    /// multi-key in progress, and a miss also clears the count — a number
    /// stuck to the next keystroke is the worst failure this mechanism can
    /// have).
    pub fn reset(&mut self) {
        self.pending.clear();
        self.count = None;
    }

    /// Pushes a key. With a sequence or count pending, `Esc` ALWAYS
    /// cancels (never runs a binding); with nothing pending, `Esc` is just
    /// another key. A bare digit accumulates into the count when the
    /// preset enables them and no sequence is in flight — mid-sequence, a
    /// digit is just another key.
    pub fn push(&mut self, chord: Chord) -> Resolution {
        if chord.is_bare_esc() && (!self.pending.is_empty() || self.count.is_some()) {
            self.reset();
            return Resolution::Reset;
        }
        // The last piece: zero never OPENS a count — `0` is still bindable,
        // which is what vim's "first column" key lives on. It accumulates
        // fine once the count is open, so `10` is ten.
        if self.eff.counts()
            && self.pending.is_empty()
            && let Some(d) = digit_of(chord)
            && (self.count.is_some() || d != 0)
        {
            let acc = self.count.unwrap_or(0);
            // Caps instead of overflowing: a fifth digit is DISCARDED,
            // never wraps the accumulator into a number nobody typed.
            let next = if acc > MAX_COUNT / 10 {
                acc
            } else {
                acc * 10 + d
            };
            let next = next.min(MAX_COUNT);
            self.count = Some(next);
            return Resolution::Counting(next);
        }
        self.pending.push(chord);
        match self.eff.lookup(&self.pending) {
            Lookup::Exact(run, Availability::Here) => {
                self.pending.clear();
                let count = match self.count.take() {
                    None => Count::None,
                    // The CATALOGUE is the authority on who accepts a
                    // count. A `lua:` command is not in it, so a count on
                    // one is `Ignored` — honest: there is no way to know
                    // what it would mean.
                    Some(n) if super::catalogue::lookup(run).is_some_and(|d| d.counts) => {
                        Count::Repeat(n)
                    }
                    Some(n) => Count::Ignored(n),
                };
                Resolution::Run {
                    command: run.to_owned(),
                    count,
                }
            }
            // K1 T4: the key is bound but this build cannot run what it is
            // bound to. A `Reset` here would be indistinguishable from an
            // unbound key: exactly the silence K1 eliminates.
            Lookup::Exact(run, why) => {
                self.pending.clear();
                self.count = None;
                Resolution::Unavailable {
                    command: run.to_owned(),
                    why,
                }
            }
            Lookup::Prefix => Resolution::Pending(self.pending.len()),
            Lookup::Miss => {
                self.reset();
                Resolution::Reset
            }
        }
    }
}
