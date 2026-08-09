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

/// Resultado de empujar una tecla al [`Resolver`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Secuencia completa: ejecutar este comando, `count` veces.
    Run {
        /// El comando a ejecutar.
        command: String,
        /// Qué significa para él el contador tecleado, si lo hubo.
        count: Count,
    },
    /// Prefijo válido de alguna secuencia: esperando (profundidad actual).
    Pending(usize),
    /// Se está tecleando un contador (valor actual). La status bar lo pinta:
    /// un contador que no se ve es un contador que no se puede cancelar.
    Counting(u32),
    /// La tecla SÍ está ligada, y lo que tiene ligado no puede ejecutarse
    /// aquí. El frontend lo dice; jamás se queda sin hacer nada.
    Unavailable {
        /// El comando al que la tecla está ligada.
        command: String,
        /// Por qué no puede ejecutarse.
        why: Availability,
    },
    /// Sin binding (o cancelación): estado limpio, tecla descartada.
    Reset,
}

/// Estado de resolución de UNA secuencia en curso. POSEE su keymap
/// efectivo: el hot-reload (ADR 0007) construye uno nuevo y reemplaza el
/// resolver entero.
#[derive(Debug, Clone)]
pub struct Resolver {
    eff: Effective,
    pending: Vec<Chord>,
    /// El contador tecleado hasta ahora, si el preset habilita contadores.
    count: Option<u32>,
}

impl Resolver {
    /// Resolver limpio sobre un keymap efectivo.
    #[must_use]
    pub fn new(eff: Effective) -> Self {
        Self {
            eff,
            pending: Vec::new(),
            count: None,
        }
    }

    /// La secuencia pendiente (para pintarla en la status bar).
    #[must_use]
    pub fn pending(&self) -> &[Chord] {
        &self.pending
    }

    /// El contador tecleado hasta ahora (para la status bar). `None` cuando no
    /// hay ningún dígito en vuelo.
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
    /// // Y se consume con el comando: no sobrevive a la pulsación.
    /// r.push(parse_chord("j").unwrap());
    /// assert_eq!(r.count(), None);
    /// ```
    #[must_use]
    pub fn count(&self) -> Option<u32> {
        self.count
    }

    /// El keymap efectivo que este resolver posee (G3c): la GUI lo necesita
    /// para construir las filas de la paleta de comandos
    /// (`palette::first_chord`) sin duplicar el `Effective` en un campo
    /// aparte de `NorteGui` — el resolver ya es la única fuente de verdad
    /// del keymap vigente (hot-reload lo reemplaza entero, ver el doc del
    /// tipo).
    #[must_use]
    pub fn effective(&self) -> &Effective {
        &self.eff
    }

    /// Rompe cualquier secuencia pendiente Y el contador en curso (una tecla
    /// no modelada por el frontend equivale a un miss: cancela el multi-tecla
    /// en curso, y un miss también limpia el contador — un número pegado a la
    /// siguiente pulsación es el peor fallo que este mecanismo puede tener).
    pub fn reset(&mut self) {
        self.pending.clear();
        self.count = None;
    }

    /// Empuja una tecla. Con secuencia o contador pendiente, `Esc` SIEMPRE
    /// cancela (jamás ejecuta un binding); sin nada pendiente, `Esc` es una
    /// tecla más. Un dígito suelto se acumula en el contador cuando el preset
    /// los habilita y no hay secuencia en vuelo — a mitad de secuencia, un
    /// dígito es una tecla más.
    pub fn push(&mut self, chord: Chord) -> Resolution {
        if chord.is_bare_esc() && (!self.pending.is_empty() || self.count.is_some()) {
            self.reset();
            return Resolution::Reset;
        }
        // El último conjunto: el cero jamás ABRE un contador — `0` sigue
        // siendo ligable, que es de lo que vive la tecla «primera columna» de
        // vim. Acumula sin problema una vez el contador está abierto, así que
        // `10` es diez.
        if self.eff.counts()
            && self.pending.is_empty()
            && let Some(d) = digit_of(chord)
            && (self.count.is_some() || d != 0)
        {
            let acc = self.count.unwrap_or(0);
            // Topa en vez de desbordar: un quinto dígito se DESCARTA, nunca
            // envuelve el acumulador a un número que nadie tecleó.
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
                    // El CATÁLOGO es la autoridad sobre quién acepta un
                    // contador. Un comando `lua:` no está en él, así que un
                    // contador sobre uno es `Ignored` — honesto: no podemos
                    // saber qué significaría.
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
            // K1 T4: la tecla está ligada pero esta build no puede correr lo
            // que tiene ligado. Un `Reset` aquí sería indistinguible de una
            // tecla sin ligar: exactamente el silencio que K1 elimina.
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
