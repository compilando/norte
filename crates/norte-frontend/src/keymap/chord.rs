//! The neutral key: `KeyCode`, `Mods`, `Chord`, and the TOML spelling of a
//! chord. No crossterm, no gpui — each frontend converts its native event
//! with `Chord::new`.

use super::KeymapError;

/// Código de tecla NEUTRO (espeja el set que acepta [`parse_chord`]; sin
/// dependencia de crossterm ni gpui).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// Carácter imprimible (espacio = `Char(' ')`).
    Char(char),
    /// Tecla de función F1..=F12.
    F(u8),
    /// Enter/Return.
    Enter,
    /// Tabulador.
    Tab,
    /// Escape.
    Esc,
    /// Backspace.
    Backspace,
    /// Flecha arriba.
    Up,
    /// Flecha abajo.
    Down,
    /// Flecha izquierda.
    Left,
    /// Flecha derecha.
    Right,
    /// Inicio.
    Home,
    /// Fin.
    End,
    /// Página arriba.
    PageUp,
    /// Página abajo.
    PageDown,
    /// Insert.
    Insert,
    /// Delete/Supr.
    Delete,
}

/// Modificadores NEUTROS de una tecla.
///
/// (Cuatro bools INDEPENDIENTES, no los estados de una máquina: cualquier
/// combinación es una pulsación real —`cmd+ctrl+alt+shift+f5` incluida— así
/// que no hay enum en el que colapsarlos. Es el bitset que el teclado
/// entrega; mismo criterio que `availability::Facts`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "modificadores independientes de un acorde; mismo criterio que `availability::Facts`"
)]
pub struct Mods {
    /// Ctrl.
    pub ctrl: bool,
    /// Alt.
    pub alt: bool,
    /// Shift.
    pub shift: bool,
    /// Cmd / Super / Meta. Only a frontend that can OBSERVE it ever sets it:
    /// the TUI cannot, because crossterm does not report super without
    /// `PushKeyboardEnhancementFlags`, which norte does not enable.
    pub cmd: bool,
}

/// Which physical modifier `mod+` means in THIS process. Set once at startup
/// by the frontend, before any keymap is built — the same shape as the
/// language in `norte_i18n` (a process-wide platform fact, decided once).
///
/// ```
/// use norte_frontend::keymap::{ModKey, Mods};
///
/// // Pure: the mapping is testable without the platform it describes.
/// assert!(ModKey::Ctrl.apply(Mods::default()).ctrl);
/// assert!(ModKey::Cmd.apply(Mods::default()).cmd);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModKey {
    /// `mod+` is Ctrl. The default, and the only honest answer for the TUI on
    /// every platform: crossterm cannot deliver Cmd.
    Ctrl,
    /// `mod+` is Cmd. A frontend that can observe Cmd — the GUI on macOS.
    Cmd,
}

impl ModKey {
    /// `mods` with this policy's bit set. Pure, so the mapping is testable
    /// without the platform it describes.
    ///
    /// ```
    /// use norte_frontend::keymap::{ModKey, Mods};
    ///
    /// let m = ModKey::Cmd.apply(Mods { alt: true, ..Mods::default() });
    /// assert!(m.cmd && m.alt, "solo AÑADE el bit de la política");
    /// assert!(!m.ctrl);
    /// ```
    #[must_use]
    pub fn apply(self, mods: Mods) -> Mods {
        match self {
            Self::Ctrl => Mods { ctrl: true, ..mods },
            Self::Cmd => Mods { cmd: true, ..mods },
        }
    }
}

static MOD_KEY: std::sync::OnceLock<ModKey> = std::sync::OnceLock::new();

/// Fix what `mod+` means. Call once, at startup, BEFORE building any keymap.
/// Returns `false` if the policy was already fixed to a different value —
/// never panics, never silently changes a keymap that is already resolved.
///
/// There is deliberately NO reset: the value is read by [`parse_chord`], so
/// changing it mid-run would make two keymaps built from the same file
/// disagree. That also means no TEST may call this with anything but the
/// current value — the policy is process-wide and a test that changed it
/// would poison every later test in the same binary. That includes DOCTESTS:
/// since the 2024 edition they are merged into ONE binary and share this
/// `OnceLock`. Test [`ModKey::apply`], which is pure.
///
/// ```
/// use norte_frontend::keymap::{ModKey, mod_key, set_mod_key};
///
/// // Setting it to what it already is (or to the default nobody set) is a
/// // no-op that reports success.
/// assert!(set_mod_key(mod_key()));
/// ```
///
/// `#[must_use]` on purpose: the bool is the ONLY signal that the policy was
/// already fixed to something else, and discarding it silently is the exact
/// pattern this work exists to remove.
#[must_use]
pub fn set_mod_key(k: ModKey) -> bool {
    *MOD_KEY.get_or_init(|| k) == k
}

/// The active policy; [`ModKey::Ctrl`] if nobody set one.
///
/// ```
/// use norte_frontend::keymap::{ModKey, mod_key};
///
/// // No frontend runs inside a doctest, so nobody has fixed the policy.
/// assert_eq!(mod_key(), ModKey::Ctrl);
/// ```
#[must_use]
pub fn mod_key() -> ModKey {
    *MOD_KEY.get_or_init(|| ModKey::Ctrl)
}

/// Una tecla con modificadores, en forma canónica. Los frontends la
/// construyen con [`Chord::new`] desde su evento nativo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    mods: Mods,
    code: KeyCode,
}

impl Chord {
    /// Chord canónico. En una tecla `Char` el carácter YA codifica shift, así
    /// que el modificador `shift` se descarta (paridad con el `from_event`
    /// crossterm de la TUI); en el resto se conserva.
    #[must_use]
    pub fn new(mods: Mods, code: KeyCode) -> Self {
        let mods = if matches!(code, KeyCode::Char(_)) {
            Mods {
                shift: false,
                ..mods
            }
        } else {
            mods
        };
        Self { mods, code }
    }

    /// The chord's parts, for the few callers that must INSPECT one rather
    /// than compare it (K2a's count accumulator asks "is this a bare digit?").
    /// It is deliberately not a pair of getters: a caller that has to reason
    /// about a chord needs both halves at once, and splitting them invites
    /// checking the key without checking the modifiers — which is how
    /// `ctrl+5` would become a count.
    ///
    /// ```
    /// use norte_frontend::keymap::{KeyCode, Mods, parse_chord};
    ///
    /// let (mods, code) = parse_chord("5").unwrap().parts();
    /// assert_eq!(mods, Mods::default());
    /// assert_eq!(code, KeyCode::Char('5'));
    ///
    /// let (mods, code) = parse_chord("ctrl+5").unwrap().parts();
    /// assert!(mods.ctrl, "un dígito con modificador jamás fue un contador");
    /// assert_eq!(code, KeyCode::Char('5'));
    /// ```
    #[must_use]
    pub fn parts(self) -> (Mods, KeyCode) {
        (self.mods, self.code)
    }

    pub(super) fn is_bare_esc(self) -> bool {
        self.code == KeyCode::Esc && self.mods == Mods::default()
    }
}

impl std::fmt::Display for Chord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `cmd` FIRST: a chord carrying both must have exactly one spelling,
        // or `parse(display(c)) == c` stops closing.
        if self.mods.cmd {
            f.write_str("cmd+")?;
        }
        if self.mods.ctrl {
            f.write_str("ctrl+")?;
        }
        if self.mods.alt {
            f.write_str("alt+")?;
        }
        if self.mods.shift {
            f.write_str("shift+")?;
        }
        match self.code {
            KeyCode::Char(' ') => f.write_str("space"),
            KeyCode::Char('+') => f.write_str("plus"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::F(n) => write!(f, "f{n}"),
            KeyCode::Enter => f.write_str("enter"),
            KeyCode::Tab => f.write_str("tab"),
            KeyCode::Esc => f.write_str("esc"),
            KeyCode::Backspace => f.write_str("backspace"),
            KeyCode::Up => f.write_str("up"),
            KeyCode::Down => f.write_str("down"),
            KeyCode::Left => f.write_str("left"),
            KeyCode::Right => f.write_str("right"),
            KeyCode::Home => f.write_str("home"),
            KeyCode::End => f.write_str("end"),
            KeyCode::PageUp => f.write_str("pgup"),
            KeyCode::PageDown => f.write_str("pgdn"),
            KeyCode::Insert => f.write_str("insert"),
            KeyCode::Delete => f.write_str("delete"),
        }
    }
}

/// Turns a chord as [`Chord`]'s `Display` writes it into text fit to be
/// PAINTED at a reader: masked first, then spelled the way every convention —
/// norte's own documentation included — spells a key (`F5`, `Shift+F8`,
/// `Ctrl+K`), instead of the raw lower case `Display` produces.
///
/// `Display` is raw and lower case ON PURPOSE: logs and debug output want the
/// literal chord, byte for byte. This is the other side — the one place a
/// chord becomes text on a terminal or a GPU surface. Both frontends route
/// through it (`norte_frontend::palette::first_chord`, the TUI's F1
/// cheatsheet, and every overlay footer), so a key is spelled one way across
/// the whole product.
///
/// # Masking runs FIRST, and the cosmetics can never undo it
///
/// [`parse_chord`] accepts ANY lone codepoint as a [`KeyCode::Char`], and a
/// project `./.norte/keymap.toml` in a cloned repository is an UNTRUSTED
/// layer: it can bind `RLO`, `BEL` or `ZWSP` to a supported command. So
/// `norte_encoding::mask_terminal_hazards` runs before anything cosmetic, and
/// the prettifier that follows it cannot resurrect what masking removed: it
/// only ever REPLACES a token it recognises with a fixed ASCII constant from
/// the table below, and passes every other token through unchanged. It never
/// decodes, unescapes, or maps a codepoint back.
///
/// # The table
///
/// A chord may be a SEQUENCE of keys joined by spaces (`g g`) and each key a
/// stack of modifiers joined by `+` — neither separator can occur inside a
/// token, because `Display` writes `Char(' ')` as `space` and `Char('+')` as
/// `plus`. Each token is then mapped:
///
/// - function keys: `f1`…`f12` → `F1`…`F12` (any `f` followed by digits);
/// - modifiers: `ctrl`/`alt`/`shift`/`cmd`/`super`/`meta` → `Ctrl`/`Alt`/
///   `Shift`/`Cmd`/`Super`/`Meta`, the `+` joins preserved. `mod` is NOT in
///   the table: it is an input alias [`parse_chord`] resolves, so `Display`
///   already wrote the physical modifier it became;
/// - named keys: `enter`, `tab`, `esc`, `backspace`, `space`, `plus`, `up`,
///   `down`, `left`, `right`, `home`, `end`, `pgup`, `pgdn`, `insert`,
///   `delete` → `Enter`, `Tab`, `Esc`, `Backspace`, `Space`, `Plus`, `Up`,
///   `Down`, `Left`, `Right`, `Home`, `End`, `PgUp`, `PgDn`, `Insert`,
///   `Delete`;
/// - a single printable character stays EXACTLY as it is: `y` must not become
///   `Y`, because the key bound is the lower-case one and telling a reader to
///   press `Y` is telling them to press Shift. This holds UNDER a modifier
///   too, and there it is not merely cosmetic: `Chord::new` drops `shift` on a
///   `Char`, and [`parse_chord`] rejects `shift+<char>` outright, so `ctrl+K`
///   is a genuinely DIFFERENT binding from `ctrl+k` (`Char('K')` vs
///   `Char('k')`) — printing `Ctrl+K` for the latter would name a chord the
///   reader does not have;
/// - an upper-case ASCII letter UNDER a modifier gets an explicit `Shift+`:
///   `alt+C` prints `Alt+Shift+C`. The stored chord carries Shift in the
///   letter's case, but a reader shown `Alt+C` cannot tell whether the case
///   matters — and it does, `alt+c` is usually another command. Only under a
///   modifier: a bare `Y` already reads as "the capital";
/// - anything else passes through untouched. Nothing is invented.
///
/// This is DISPLAY only. The keymap, [`parse_chord`] and every stored string
/// stay byte-identical.
///
/// ```
/// use norte_frontend::keymap::paint_chord;
///
/// assert_eq!(paint_chord("f5"), "F5");
/// assert_eq!(paint_chord("shift+f8"), "Shift+F8");
/// assert_eq!(paint_chord("ctrl+k"), "Ctrl+k");
/// assert_eq!(paint_chord("y"), "y", "the bound key is the lower-case one");
/// assert_eq!(paint_chord("g g"), "g g");
/// // An upper-case letter UNDER a modifier is Shift, and the label says so:
/// // `Alt+C` alone leaves the reader guessing whether the case matters.
/// assert_eq!(paint_chord("alt+C"), "Alt+Shift+C");
/// assert_eq!(paint_chord("ctrl+alt+K"), "Ctrl+Alt+Shift+K");
/// assert_eq!(paint_chord("Y"), "Y", "alone, the letter is the whole story");
/// ```
#[must_use]
pub fn paint_chord(raw: &str) -> String {
    // Masking FIRST — see the rustdoc above. Never reorder these two.
    let masked = norte_encoding::mask_terminal_hazards(raw);
    let mut out = String::with_capacity(masked.len());
    for (i, key) in masked.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let tokens: Vec<&str> = key.split('+').collect();
        for (j, token) in tokens.iter().enumerate() {
            if j > 0 {
                out.push('+');
            }
            // The letter's case carries Shift in the stored chord (see
            // `parse_chord`); a reader is told to press it. Only under a
            // modifier: a bare `Y` already reads as "the capital".
            if j == tokens.len() - 1 && j > 0 && es_letra_mayuscula(token) {
                out.push_str("Shift+");
            }
            out.push_str(&pretty_token(token));
        }
    }
    out
}

/// Un token que es exactamente una letra ASCII mayúscula.
fn es_letra_mayuscula(token: &str) -> bool {
    let mut chars = token.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if c.is_ascii_uppercase())
}

/// One token of a chord, spelled for a reader. See [`paint_chord`] for the
/// whole table and for why nothing here can undo the masking that ran before.
fn pretty_token(token: &str) -> String {
    let named = match token {
        "ctrl" => "Ctrl",
        "alt" => "Alt",
        "shift" => "Shift",
        "cmd" => "Cmd",
        // Not spellings `Display` produces today (`Mods` has no such flag),
        // but a frontend that grows them must not have to touch this table.
        // `mod` is deliberately ABSENT: it is an INPUT alias resolved by
        // `parse_chord`, so `Display` writes the physical modifier it became
        // (`ctrl`/`cmd`) and a reader is told the key they actually press,
        // never a name they cannot find on their keyboard.
        "super" => "Super",
        "meta" => "Meta",
        "enter" => "Enter",
        "tab" => "Tab",
        "esc" => "Esc",
        "backspace" => "Backspace",
        "space" => "Space",
        "plus" => "Plus",
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        "home" => "Home",
        "end" => "End",
        "pgup" => "PgUp",
        "pgdn" => "PgDn",
        "insert" => "Insert",
        "delete" => "Delete",
        other => {
            // `f` plus digits is a function key, and ONLY that: a lone
            // `Char` token is exactly one character, so `f5` can never be a
            // character binding. The digits are copied verbatim.
            let rest = other.strip_prefix('f').unwrap_or_default();
            if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
                return format!("F{rest}");
            }
            // A single printable character, or anything unrecognised: raw.
            return other.to_owned();
        }
    };
    named.to_owned()
}

/// Parsea `"ctrl+alt+x"`, `"f5"`, `"g"`, `"shift+f5"`, `"esc"`, `"plus"`…
/// `+` es el separador de modificadores, así que `plus` es la ÚNICA forma de
/// expresar esa tecla.
///
/// # `mod+` y `cmd+`
///
/// `cmd` es literal (Cmd/Super/Meta), para un preset que quiere decir Cmd y
/// nada más. `mod` es el ALIAS por-OS: se resuelve a lo que [`mod_key`] diga
/// en ESTE proceso — Ctrl por defecto, Cmd si la política lo fijó — de modo
/// que un preset sigue siendo UN fichero en macOS y en Linux/Windows. El
/// alias cuenta como aquello en lo que se resuelve para el rechazo de
/// modificador repetido: bajo la política Ctrl, `ctrl+mod+x` es la misma
/// tecla dos veces y es [`KeymapError::BadChord`].
///
/// # Errors
/// [`KeymapError::BadChord`] si el texto no describe una tecla.
pub fn parse_chord(s: &str) -> Result<Chord, KeymapError> {
    let bad = || KeymapError::BadChord {
        chord: s.to_owned(),
    };
    let parts: Vec<&str> = s.split('+').collect();
    let (mods_txt, key_txt) = parts.split_at(parts.len().saturating_sub(1));
    let key_txt = key_txt
        .first()
        .copied()
        .filter(|k| !k.is_empty())
        .ok_or_else(bad)?;
    // `cmd` y `mod` JUNTOS se rechazan SIEMPRE, mire quien mire (rust-reviewer
    // MINOR-10). Bajo la política Cmd son la misma tecla dos veces y el
    // rechazo de modificador repetido de abajo ya los mataría; bajo Ctrl no,
    // y el resultado sería un chord que carga en Linux y revienta en macOS —
    // la asimetría exacta que la decisión 8 del ADR 0043 dice evitar,
    // descubierta por quien corre la plataforma donde está rota. Que un
    // chord sea válido o no NO puede depender del sistema operativo.
    if mods_txt.contains(&"cmd") && mods_txt.contains(&"mod") {
        return Err(bad());
    }
    let mut mods = Mods::default();
    for m in mods_txt {
        let slot = match *m {
            "ctrl" => &mut mods.ctrl,
            "alt" => &mut mods.alt,
            "shift" => &mut mods.shift,
            "cmd" => &mut mods.cmd,
            // `mod` is the alias: whichever physical modifier this process
            // decided at startup. One preset file, two platforms.
            "mod" => match mod_key() {
                ModKey::Ctrl => &mut mods.ctrl,
                ModKey::Cmd => &mut mods.cmd,
            },
            _ => return Err(bad()),
        };
        if *slot {
            // Modificador repetido. El alias cuenta como AQUELLO en lo que se
            // resuelve, así que `ctrl+mod+x` con la política Ctrl es la misma
            // tecla dos veces y muere aquí — correcto.
            return Err(bad());
        }
        *slot = true;
    }
    let code = match key_txt {
        "enter" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "esc" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        // `+` es el SEPARADOR de modificadores, así que un token "+" da key
        // vacía y muere en BadChord: `plus` es la única forma de expresar la
        // tecla (#103, mark.pattern-add). Aditivo: ningún keymap de usuario
        // podía contener "+" como tecla, porque hoy no parsea.
        "plus" => KeyCode::Char('+'),
        "backspace" => KeyCode::Backspace,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pgup" => KeyCode::PageUp,
        "pgdn" => KeyCode::PageDown,
        "insert" => KeyCode::Insert,
        "delete" => KeyCode::Delete,
        f if f.len() >= 2 && f.starts_with('f') => {
            let n: u8 = f[1..].parse().map_err(|_| bad())?;
            if (1..=12).contains(&n) {
                KeyCode::F(n)
            } else {
                return Err(bad());
            }
        }
        c => {
            let mut chars = c.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) => KeyCode::Char(ch),
                _ => return Err(bad()),
            }
        }
    };
    if mods.shift && matches!(code, KeyCode::Char(_)) {
        return Err(KeymapError::ShiftWithChar {
            chord: s.to_owned(),
        });
    }
    // OJO: NO usar Chord::new aquí (descartaría el shift antes del check de
    // arriba); el check ya rechazó shift+Char, y para el resto shift se
    // conserva. Construye el Chord directo:
    Ok(Chord { mods, code })
}
