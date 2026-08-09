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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods {
    /// Ctrl.
    pub ctrl: bool,
    /// Alt.
    pub alt: bool,
    /// Shift.
    pub shift: bool,
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

    pub(super) fn is_bare_esc(self) -> bool {
        self.code == KeyCode::Esc && self.mods == Mods::default()
    }
}

impl std::fmt::Display for Chord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
/// - modifiers: `ctrl`/`alt`/`shift`/`super`/`meta` → `Ctrl`/`Alt`/`Shift`/
///   `Super`/`Meta`, the `+` joins preserved;
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
        for (j, token) in key.split('+').enumerate() {
            if j > 0 {
                out.push('+');
            }
            out.push_str(&pretty_token(token));
        }
    }
    out
}

/// One token of a chord, spelled for a reader. See [`paint_chord`] for the
/// whole table and for why nothing here can undo the masking that ran before.
fn pretty_token(token: &str) -> String {
    let named = match token {
        "ctrl" => "Ctrl",
        "alt" => "Alt",
        "shift" => "Shift",
        // Not spellings `Display` produces today (`Mods` has no such flag),
        // but a frontend that grows them must not have to touch this table.
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
    let mut mods = Mods::default();
    for m in mods_txt {
        let slot = match *m {
            "ctrl" => &mut mods.ctrl,
            "alt" => &mut mods.alt,
            "shift" => &mut mods.shift,
            _ => return Err(bad()),
        };
        if *slot {
            return Err(bad()); // modificador repetido
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
