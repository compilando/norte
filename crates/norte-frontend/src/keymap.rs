//! Motor de keymap PURO compartido por los frontends (ADR 0006/0007): mapa
//! `(contexto, secuencia) → comando`, capas estilo Yazi, efectivo prefix-free
//! validado al cargar — la resolución es un scan lineal determinista sobre el
//! efectivo (≤ centenas de bindings), sin timeouts. Tecla NEUTRA (sin
//! crossterm/gpui): cada frontend convierte su evento nativo a [`Chord`] con
//! [`Chord::new`].

use std::collections::HashSet;

use serde::Deserialize;

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

    fn is_bare_esc(self) -> bool {
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

/// Error de carga o parseo de un keymap. Diagnóstico SIEMPRE accionable:
/// la config rota es un error claro, jamás comportamiento raro.
#[derive(Debug, thiserror::Error)]
pub enum KeymapError {
    /// El TOML no parsea o tiene claves desconocidas.
    #[error("keymap.toml inválido: {0}")]
    Toml(String),
    /// Una tecla no se entiende (`"megatecla"`, `"ctrl+"`, `"f99"`).
    #[error("tecla inválida: {chord:?}")]
    BadChord {
        /// El texto que no parseó.
        chord: String,
    },
    /// Un binding con secuencia vacía.
    #[error("binding con secuencia vacía (run = {run:?})")]
    EmptySequence {
        /// El comando del binding vacío.
        run: String,
    },
    /// El comando no existe (typo o versión vieja).
    #[error("comando desconocido: {run:?}")]
    UnknownCommand {
        /// El nombre que no se reconoce.
        run: String,
    },
    /// `shift+<char>` jamás matchearía (el char YA codifica shift): se
    /// rechaza con diagnóstico en vez de ser un binding muerto.
    #[error("{chord:?}: shift no se combina con caracteres — usa la mayúscula (\"G\")")]
    ShiftWithChar {
        /// El texto ofensor.
        chord: String,
    },
    /// La capa trae la lista equivocada: un preset define `keymap`; la capa
    /// de usuario define `prepend_keymap`/`append_keymap` (modelo Yazi).
    /// Ignorarlo en silencio sería config rota sin error.
    #[error("la capa {layer} no admite {key} (preset: keymap; usuario: prepend/append)")]
    WrongLayerKey {
        /// `"preset"` o `"usuario"`.
        layer: &'static str,
        /// La clave que sobra.
        key: &'static str,
    },
    /// `esc` dentro de una secuencia multi-tecla: inalcanzable, porque
    /// `Esc` SIEMPRE cancela un pendiente (solo vale como binding suelto).
    #[error("esc solo puede ligarse como tecla suelta, no dentro de {sequence:?}")]
    EscInSequence {
        /// La secuencia ofensora.
        sequence: String,
    },
    /// Una secuencia es prefijo estricto de otra: prohibido (ADR 0006 —
    /// sin timeouts, la resolución debe ser determinista).
    #[error("secuencias ambiguas: {shorter:?} es prefijo de {longer:?}")]
    AmbiguousPrefix {
        /// La secuencia corta (la que se dispararía siempre).
        shorter: String,
        /// La secuencia larga (la inalcanzable).
        longer: String,
    },
}

/// Parsea `"ctrl+alt+x"`, `"f5"`, `"g"`, `"shift+f5"`, `"esc"`…
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

/// Un binding tal como viene del TOML.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct RawBinding {
    on: Vec<String>,
    run: String,
}

/// Las tres listas de una sección (preset: `keymap`; usuario:
/// `prepend_keymap`/`append_keymap` — modelo Yazi, spec §12).
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct RawSection {
    #[serde(default)]
    keymap: Vec<RawBinding>,
    #[serde(default)]
    prepend_keymap: Vec<RawBinding>,
    #[serde(default)]
    append_keymap: Vec<RawBinding>,
}

/// Un `keymap.toml` parseado (preset de fábrica o capa de usuario).
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct KeymapFile {
    #[serde(default)]
    global: RawSection,
    #[serde(default)]
    pane: RawSection,
    #[serde(default)]
    viewer: RawSection,
    /// `true` si esta capa es la de PROYECTO (`./.norte`) — contenido
    /// potencialmente AJENO (viene con un repo clonado) que se carga SIN
    /// trust. Un keymap de proyecto NO puede bindear `lua:`:
    /// [`Effective::build_for`] descarta esos bindings (contados en
    /// [`Effective::discarded_lua_bindings`]) — rebindear una tecla común a
    /// un comando del `init.lua` del USUARIO (sin sandbox) sería ejecución
    /// dirigida por el repo sin confirmación alguna. No viene del TOML
    /// (`serde(skip)`): lo marca `config::load` por posición de capa
    /// (deuda #75: `Layers` debería llevar el kind por dir).
    #[serde(skip)]
    project: bool,
}

impl KeymapFile {
    /// ¿Define `keymap` (lista completa de preset)? Las CAPAS de usuario
    /// no lo admiten — el diagnóstico con archivo vive en `config::load`.
    #[must_use]
    pub fn has_full_keymap(&self) -> bool {
        !self.global.keymap.is_empty()
            || !self.pane.keymap.is_empty()
            || !self.viewer.keymap.is_empty()
    }

    /// Marca esta capa como la de PROYECTO (ver el campo `project`): sus
    /// bindings `lua:` se descartan al fusionar. La llama `config::load`
    /// con el `keymap.toml` de `./.norte`.
    pub fn mark_project(&mut self) {
        self.project = true;
    }

    /// ¿Es la capa de proyecto? (ver [`Self::mark_project`]).
    #[must_use]
    pub fn is_project(&self) -> bool {
        self.project
    }
}

/// Pantalla activa: decide qué contexto específico se fusiona con
/// `global` (ADR 0006; el stack crece con la UI — dialog es issue #24).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Los dos panes (contexto `pane`).
    Browse,
    /// El viewer (contexto `viewer`, fase 7).
    Viewer,
}

/// Diagnóstico compacto de un error de parseo TOML: `"line N: msg"` si el
/// error trae span, o solo el mensaje si no (errores semánticos). Copia
/// LOCAL de `norte_tui::config::toml_diag` — el motor no depende de la TUI.
fn toml_diag(raw: &str, e: &toml::de::Error) -> String {
    match e.span() {
        Some(s) => {
            let line = 1 + raw[..s.start.min(raw.len())].matches('\n').count();
            format!("line {line}: {}", e.message())
        }
        None => e.message().to_owned(),
    }
}

/// Parsea un `keymap.toml`.
///
/// # Errors
/// [`KeymapError::Toml`] si no parsea o hay claves desconocidas.
pub fn parse_keymap(s: &str) -> Result<KeymapFile, KeymapError> {
    toml::from_str(s).map_err(|e| KeymapError::Toml(toml_diag(s, &e)))
}

/// Resultado de empujar una tecla al [`Resolver`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Secuencia completa: ejecutar este comando.
    Run(String),
    /// Prefijo válido de alguna secuencia: esperando (profundidad actual).
    Pending(usize),
    /// Sin binding (o cancelación): estado limpio, tecla descartada.
    Reset,
}

/// Keymap EFECTIVO: capas y contextos ya fusionados y validados
/// (prefix-free). Inmutable tras construir; clonable barato (el hot-reload
/// construye uno nuevo y lo cambia entero, ADR 0007).
#[derive(Debug, Clone)]
pub struct Effective {
    bindings: Vec<(Vec<Chord>, String)>,
    /// Bindings `lua:` DESCARTADOS por venir de la capa de proyecto
    /// (seguridad, ver [`KeymapFile::mark_project`]). El frontend lo avisa una
    /// vez (jamás descarte mudo); el mensaje concreto es cosa del frontend.
    discarded_lua_bindings: usize,
}

/// Fusión de un contexto (ADR 0006/0007): prepends de capa superior primero
/// (ganan), luego el preset, luego los appends (superiores antes). Los
/// bindings `lua:` de una capa de PROYECTO se DESCARTAN aquí, contados en
/// `discarded_lua` (seguridad: ver [`KeymapFile::mark_project`] — el
/// keymap de un repo ajeno no puede dirigir la ejecución de comandos Lua).
fn merge_ctx<'a>(
    preset: &'a KeymapFile,
    layers: &'a [KeymapFile],
    get: fn(&KeymapFile) -> &RawSection,
    discarded_lua: &mut usize,
) -> Vec<&'a RawBinding> {
    let mut out = Vec::new();
    let mut push = |layer_project: bool, b: &'a RawBinding, out: &mut Vec<&'a RawBinding>| {
        if layer_project && b.run.starts_with("lua:") {
            *discarded_lua += 1;
        } else {
            out.push(b);
        }
    };
    for l in layers.iter().rev() {
        for b in &get(l).prepend_keymap {
            push(l.project, b, &mut out);
        }
    }
    out.extend(get(preset).keymap.iter());
    for l in layers.iter().rev() {
        for b in &get(l).append_keymap {
            push(l.project, b, &mut out);
        }
    }
    out
}

/// Charset de un nombre de comando Lua (`lua:<nombre>`). Espeja
/// `norte_tui::lua::valid_name`; el motor lo usa solo para VALIDAR el
/// binding, no ejecuta Lua (eso es del frontend que tenga host).
fn valid_lua_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

impl Effective {
    /// Fusiona `preset` + capa opcional de usuario (ADR 0006). Azúcar de
    /// [`Self::build_layered`] con cero o una capa.
    ///
    /// # Errors
    /// Ver [`KeymapError`] — todos son errores de CARGA con diagnóstico.
    pub fn build(
        preset: &KeymapFile,
        user: Option<&KeymapFile>,
        known_commands: &[&str],
    ) -> Result<Self, KeymapError> {
        match user {
            Some(u) => Self::build_layered(preset, std::slice::from_ref(u), known_commands),
            None => Self::build_layered(preset, &[], known_commands),
        }
    }

    /// Fusiona `preset` + N capas de usuario en precedencia ASCENDENTE
    /// (sistema → usuario → proyecto, ADR 0007) y valida (ADR 0006): por
    /// contexto, los `prepend` de capas superiores van primero (ganan),
    /// luego el preset, luego los `append` (superiores antes); entre
    /// contextos, el específico (`pane`) pisa al `global`; el resultado
    /// debe ser prefix-free y con comandos conocidos.
    ///
    /// # Errors
    /// Ver [`KeymapError`] — todos son errores de CARGA con diagnóstico.
    pub fn build_layered(
        preset: &KeymapFile,
        layers: &[KeymapFile],
        known_commands: &[&str],
    ) -> Result<Self, KeymapError> {
        Self::build_for(preset, layers, known_commands, Screen::Browse)
    }

    /// Fusiona para una pantalla concreta: su contexto específico pisa a
    /// `global` por secuencia exacta (ADR 0006), capas como en
    /// [`Self::build_layered`].
    ///
    /// # Errors
    /// Ver [`KeymapError`].
    pub fn build_for(
        preset: &KeymapFile,
        layers: &[KeymapFile],
        known_commands: &[&str],
        screen: Screen,
    ) -> Result<Self, KeymapError> {
        // Cada capa admite SOLO sus listas (revisión fase 4): descartar en
        // silencio la lista equivocada sería el "comportamiento raro" que
        // el ADR prohíbe.
        for section in [&preset.global, &preset.pane, &preset.viewer] {
            if !(section.prepend_keymap.is_empty() && section.append_keymap.is_empty()) {
                return Err(KeymapError::WrongLayerKey {
                    layer: "preset",
                    key: "prepend_keymap/append_keymap",
                });
            }
        }
        for layer in layers {
            for section in [&layer.global, &layer.pane, &layer.viewer] {
                if !section.keymap.is_empty() {
                    return Err(KeymapError::WrongLayerKey {
                        layer: "usuario",
                        key: "keymap",
                    });
                }
            }
        }
        // Entre contextos: el específico de la pantalla antes que global.
        // La fusión (con el descarte de `lua:` de la capa de proyecto) vive
        // en [`merge_ctx`].
        let specific: fn(&KeymapFile) -> &RawSection = match screen {
            Screen::Browse => |f| &f.pane,
            Screen::Viewer => |f| &f.viewer,
        };
        let mut discarded_lua_bindings = 0usize;
        let ordered: Vec<&RawBinding> =
            merge_ctx(preset, layers, specific, &mut discarded_lua_bindings)
                .into_iter()
                .chain(merge_ctx(
                    preset,
                    layers,
                    |f| &f.global,
                    &mut discarded_lua_bindings,
                ))
                .collect();

        let mut seen: HashSet<Vec<Chord>> = HashSet::new();
        let mut bindings: Vec<(Vec<Chord>, String)> = Vec::new();
        for raw in ordered {
            let seq: Vec<Chord> = raw
                .on
                .iter()
                .map(|s| parse_chord(s))
                .collect::<Result<_, _>>()?;
            if seq.is_empty() {
                return Err(KeymapError::EmptySequence {
                    run: raw.run.clone(),
                });
            }
            // Esc es la cancelación de secuencia (lo cazó el proptest: un
            // esc no-inicial sería inalcanzable): solo como binding suelto.
            if seq.len() > 1 && seq.iter().any(|c| c.is_bare_esc()) {
                return Err(KeymapError::EscInSequence {
                    sequence: format!("{:?}", raw.on),
                });
            }
            // `lua:<nombre>` (M4 Lua, T8): el registro de comandos Lua es
            // DINÁMICO (runtime), así que no se valida contra
            // `known_commands` — solo el charset del nombre (la MISMA
            // `valid_lua_name`, una sola fuente). Un comando lua no
            // registrado al invocar NO es error de keymap: el frontend con
            // host avisa en runtime.
            if let Some(lua_name) = raw.run.strip_prefix("lua:") {
                if !valid_lua_name(lua_name) {
                    return Err(KeymapError::UnknownCommand {
                        run: raw.run.clone(),
                    });
                }
            } else if !known_commands.contains(&raw.run.as_str()) {
                return Err(KeymapError::UnknownCommand {
                    run: raw.run.clone(),
                });
            }
            // El primero gana (el orden YA codifica la precedencia).
            if seen.insert(seq.clone()) {
                bindings.push((seq, raw.run.clone()));
            }
        }

        // Prefix-free: ninguna secuencia es prefijo estricto de otra.
        for (i, (a, _)) in bindings.iter().enumerate() {
            for (b, _) in bindings.iter().skip(i + 1) {
                let (short, long) = if a.len() < b.len() { (a, b) } else { (b, a) };
                if short.len() < long.len() && long[..short.len()] == short[..] {
                    return Err(KeymapError::AmbiguousPrefix {
                        shorter: format!("{short:?}"),
                        longer: format!("{long:?}"),
                    });
                }
            }
        }
        Ok(Self {
            bindings,
            discarded_lua_bindings,
        })
    }

    /// Bindings `lua:` descartados por venir de la capa de PROYECTO (`./
    /// .norte`, sin trust — seguridad, ver [`KeymapFile::mark_project`]).
    /// El caller (main) lo pinta una vez por barra; los rebinds de proyecto
    /// a builtins NO cuentan aquí (siguen funcionando).
    #[must_use]
    pub fn discarded_lua_bindings(&self) -> usize {
        self.discarded_lua_bindings
    }

    /// Los bindings efectivos, en orden de precedencia: secuencia ya
    /// formateada (`"g g"`, `"ctrl+k"`) y comando. La AYUDA se construye
    /// de aquí — refleja preset y capas del usuario, jamás listas a mano.
    #[must_use]
    pub fn bindings(&self) -> Vec<(String, &str)> {
        self.bindings
            .iter()
            .map(|(seq, run)| {
                let teclas: Vec<String> = seq.iter().map(ToString::to_string).collect();
                (teclas.join(" "), run.as_str())
            })
            .collect()
    }

    fn lookup(&self, candidate: &[Chord]) -> Lookup<'_> {
        for (seq, run) in &self.bindings {
            if seq[..] == candidate[..] {
                return Lookup::Exact(run);
            }
        }
        if self
            .bindings
            .iter()
            .any(|(seq, _)| seq.len() > candidate.len() && seq[..candidate.len()] == candidate[..])
        {
            Lookup::Prefix
        } else {
            Lookup::Miss
        }
    }
}

enum Lookup<'a> {
    Exact(&'a str),
    Prefix,
    Miss,
}

/// Estado de resolución de UNA secuencia en curso. POSEE su keymap
/// efectivo: el hot-reload (ADR 0007) construye uno nuevo y reemplaza el
/// resolver entero.
#[derive(Debug, Clone)]
pub struct Resolver {
    eff: Effective,
    pending: Vec<Chord>,
}

impl Resolver {
    /// Resolver limpio sobre un keymap efectivo.
    #[must_use]
    pub fn new(eff: Effective) -> Self {
        Self {
            eff,
            pending: Vec::new(),
        }
    }

    /// La secuencia pendiente (para pintarla en la status bar).
    #[must_use]
    pub fn pending(&self) -> &[Chord] {
        &self.pending
    }

    /// Rompe cualquier secuencia pendiente (una tecla no modelada por el
    /// frontend equivale a un miss: cancela el multi-tecla en curso).
    pub fn reset(&mut self) {
        self.pending.clear();
    }

    /// Empuja una tecla. Con secuencia pendiente, `Esc` SIEMPRE cancela
    /// (jamás ejecuta un binding); sin pendiente, `Esc` es una tecla más.
    pub fn push(&mut self, chord: Chord) -> Resolution {
        if !self.pending.is_empty() && chord.is_bare_esc() {
            self.pending.clear();
            return Resolution::Reset;
        }
        self.pending.push(chord);
        match self.eff.lookup(&self.pending) {
            Lookup::Exact(run) => {
                self.pending.clear();
                Resolution::Run(run.to_owned())
            }
            Lookup::Prefix => Resolution::Pending(self.pending.len()),
            Lookup::Miss => {
                self.pending.clear();
                Resolution::Reset
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eff(preset: &str, user: Option<&str>) -> Result<Effective, KeymapError> {
        const COMANDOS: &[&str] = &[
            "app.quit",
            "pane.switch",
            "cursor.up",
            "cursor.down",
            "cursor.top",
            "cursor.bottom",
            "nav.enter",
        ];
        let preset = parse_keymap(preset)?;
        let user = user.map(parse_keymap).transpose()?;
        Effective::build(&preset, user.as_ref(), COMANDOS)
    }

    #[test]
    fn parse_de_chords() {
        assert_eq!(
            parse_chord("f5").unwrap(),
            Chord::new(Mods::default(), KeyCode::F(5))
        );
        assert_eq!(
            parse_chord("ctrl+c").unwrap(),
            Chord::new(
                Mods {
                    ctrl: true,
                    ..Default::default()
                },
                KeyCode::Char('c')
            )
        );
        assert_eq!(
            parse_chord("alt+enter").unwrap(),
            Chord::new(
                Mods {
                    alt: true,
                    ..Default::default()
                },
                KeyCode::Enter
            )
        );
        // Mayúscula: el char YA codifica shift.
        assert_eq!(
            parse_chord("G").unwrap(),
            Chord::new(Mods::default(), KeyCode::Char('G'))
        );
        assert_eq!(
            parse_chord("shift+f5").unwrap(),
            Chord::new(
                Mods {
                    shift: true,
                    ..Default::default()
                },
                KeyCode::F(5)
            )
        );
        for s in ["", "ctrl+", "megatecla", "ctrl+ctrl+c", "f99"] {
            assert!(parse_chord(s).is_err(), "{s:?} debe fallar");
        }
    }

    #[test]
    fn chord_new_normaliza_shift_en_chars_pero_no_en_otras_teclas() {
        // Un evento nativo con Char('G')+shift: el chord canónico descarta
        // shift (el char ya lo codifica) — paridad con el viejo
        // `Chord::from_event` de la TUI (ahora el comportamiento por
        // defecto de `Chord::new`).
        let c = Chord::new(
            Mods {
                shift: true,
                ..Default::default()
            },
            KeyCode::Char('G'),
        );
        assert_eq!(c, Chord::new(Mods::default(), KeyCode::Char('G')));
        // En teclas no-char, shift ES información.
        let f = Chord::new(
            Mods {
                shift: true,
                ..Default::default()
            },
            KeyCode::F(5),
        );
        assert_eq!(
            f,
            Chord::new(
                Mods {
                    shift: true,
                    ..Default::default()
                },
                KeyCode::F(5)
            )
        );
    }

    #[test]
    fn resuelve_secuencias_multi_tecla() {
        let preset = r#"
            [pane]
            keymap = [
                { on = ["g", "g"], run = "cursor.top" },
                { on = ["G"], run = "cursor.bottom" },
            ]
            [global]
            keymap = [{ on = ["q"], run = "app.quit" }]
        "#;
        let eff = eff(preset, None).unwrap();
        let mut r = Resolver::new(eff.clone());
        assert_eq!(
            r.push(parse_chord("g").unwrap()),
            Resolution::Pending(1),
            "prefijo válido: espera"
        );
        assert_eq!(
            r.push(parse_chord("g").unwrap()),
            Resolution::Run("cursor.top".into())
        );
        // Tras ejecutar, el estado queda limpio.
        assert_eq!(
            r.push(parse_chord("G").unwrap()),
            Resolution::Run("cursor.bottom".into())
        );
        // Tecla sin binding: reset silencioso.
        assert_eq!(r.push(parse_chord("z").unwrap()), Resolution::Reset);
        // Prefijo pendiente + tecla que no continúa: reset (no ejecuta nada).
        r.push(parse_chord("g").unwrap());
        assert_eq!(r.push(parse_chord("q").unwrap()), Resolution::Reset);
        // q suelto (contexto global) sí corre.
        assert_eq!(
            r.push(parse_chord("q").unwrap()),
            Resolution::Run("app.quit".into())
        );
    }

    #[test]
    fn esc_cancela_la_secuencia_pendiente() {
        let preset = r#"
            [pane]
            keymap = [
                { on = ["g", "g"], run = "cursor.top" },
                { on = ["esc"], run = "app.quit" },
            ]
        "#;
        let eff = eff(preset, None).unwrap();
        let mut r = Resolver::new(eff.clone());
        r.push(parse_chord("g").unwrap());
        // Con secuencia pendiente, Esc SIEMPRE cancela (jamás ejecuta binding).
        assert_eq!(r.push(parse_chord("esc").unwrap()), Resolution::Reset);
        // Sin pendiente, Esc es una tecla más.
        assert_eq!(
            r.push(parse_chord("esc").unwrap()),
            Resolution::Run("app.quit".into())
        );
    }

    #[test]
    fn prefijo_ambiguo_es_error_de_carga() {
        let preset = r#"
            [pane]
            keymap = [
                { on = ["g"], run = "cursor.top" },
                { on = ["g", "g"], run = "cursor.bottom" },
            ]
        "#;
        match eff(preset, None) {
            Err(KeymapError::AmbiguousPrefix { .. }) => {}
            other => panic!("esperaba AmbiguousPrefix, fue {other:?}"),
        }
    }

    #[test]
    fn shift_con_char_es_error_diagnosticable() {
        // Un binding "shift+g" jamás matchearía (el chord canónico descarta
        // shift en chars): rechazo al parsear, no binding muerto.
        match parse_chord("shift+g") {
            Err(KeymapError::ShiftWithChar { .. }) => {}
            other => panic!("esperaba ShiftWithChar, fue {other:?}"),
        }
        assert!(parse_chord("ctrl+shift+c").is_err());
        // En teclas no-char, shift es legítimo.
        assert!(parse_chord("shift+f5").is_ok());
    }

    #[test]
    fn lista_equivocada_en_una_capa_es_error() {
        // Usuario con `keymap` (en vez de prepend/append): error, no silencio.
        let preset = r#"
            [pane]
            keymap = [{ on = ["j"], run = "cursor.down" }]
        "#;
        let user = r#"
            [pane]
            keymap = [{ on = ["j"], run = "cursor.up" }]
        "#;
        match eff(preset, Some(user)) {
            Err(KeymapError::WrongLayerKey {
                layer: "usuario", ..
            }) => {}
            other => panic!("esperaba WrongLayerKey usuario, fue {other:?}"),
        }
        // Preset con prepend: mismo trato.
        let preset_malo = r#"
            [pane]
            prepend_keymap = [{ on = ["j"], run = "cursor.down" }]
        "#;
        match eff(preset_malo, None) {
            Err(KeymapError::WrongLayerKey {
                layer: "preset", ..
            }) => {}
            other => panic!("esperaba WrongLayerKey preset, fue {other:?}"),
        }
    }

    #[test]
    fn la_especificidad_de_contexto_prevalece_sobre_la_capa() {
        // ADR 0006 (desambiguado en fase 4): las capas se fusionan POR
        // contexto; entre contextos gana el específico. Un append de usuario
        // en [pane] pisa al keymap [global] del preset…
        let preset = r#"
            [global]
            keymap = [{ on = ["q"], run = "app.quit" }]
            [pane]
            keymap = [{ on = ["j"], run = "cursor.down" }]
        "#;
        let user = r#"
            [pane]
            append_keymap = [{ on = ["q"], run = "cursor.up" }]
            [global]
            prepend_keymap = [{ on = ["j"], run = "app.quit" }]
        "#;
        let eff = eff(preset, Some(user)).unwrap();
        let mut r = Resolver::new(eff.clone());
        assert_eq!(
            r.push(parse_chord("q").unwrap()),
            Resolution::Run("cursor.up".into()),
            "pane.append gana a global.keymap (especificidad > capa)"
        );
        // …y un prepend de usuario en [global] NO pisa al keymap [pane].
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run("cursor.down".into()),
            "global.prepend no pisa a pane.keymap"
        );
    }

    #[test]
    fn esc_dentro_de_secuencia_es_error_de_carga() {
        let preset = r#"
            [pane]
            keymap = [{ on = ["a", "esc"], run = "cursor.up" }]
        "#;
        match eff(preset, None) {
            Err(KeymapError::EscInSequence { .. }) => {}
            other => panic!("esperaba EscInSequence, fue {other:?}"),
        }
    }

    #[test]
    fn comando_desconocido_es_error_de_carga() {
        let preset = r#"
            [pane]
            keymap = [{ on = ["x"], run = "comando.inventado" }]
        "#;
        match eff(preset, None) {
            Err(KeymapError::UnknownCommand { .. }) => {}
            other => panic!("esperaba UnknownCommand, fue {other:?}"),
        }
    }

    /// M4 Lua (T8, espejado): un binding a `lua:<nombre>` pasa la validación
    /// aunque el nombre no esté en `known_commands` — el registro Lua es
    /// dinámico (runtime); un comando lua no registrado NO es error de
    /// keymap. El NOMBRE sí se valida con el mismo charset que
    /// `norte.command` (`[a-z0-9._-]{1,64}`): un binding a un nombre que
    /// jamás podría registrarse es config rota diagnosticable, no un binding
    /// muerto en silencio.
    #[test]
    fn lua_prefijado_pasa_la_validacion_de_comandos() {
        let preset = r#"
            [pane]
            keymap = [{ on = ["x"], run = "lua:mi-comando.v2" }]
        "#;
        let mut r = Resolver::new(eff(preset, None).expect("lua: con nombre válido pasa"));
        assert_eq!(
            r.push(parse_chord("x").unwrap()),
            Resolution::Run("lua:mi-comando.v2".into()),
            "el binding resuelve al comando lua: completo"
        );

        // Nombres fuera del charset [a-z0-9._-]{1,64}: error de CARGA.
        let largo = format!("lua:{}", "a".repeat(65));
        for bad in ["lua:", "lua:Mayuscula", "lua:con espacio", largo.as_str()] {
            let preset = format!(
                r#"
                [pane]
                keymap = [{{ on = ["x"], run = "{bad}" }}]
                "#
            );
            match eff(&preset, None) {
                Err(KeymapError::UnknownCommand { .. }) => {}
                other => panic!("esperaba UnknownCommand para {bad:?}, fue {other:?}"),
            }
        }
    }

    #[test]
    fn capas_yazi_prepend_pisa_y_append_solo_anade() {
        let preset = r#"
            [pane]
            keymap = [
                { on = ["j"], run = "cursor.down" },
                { on = ["k"], run = "cursor.up" },
            ]
        "#;
        let user = r#"
            [pane]
            prepend_keymap = [{ on = ["j"], run = "cursor.top" }]
            append_keymap = [
                { on = ["k"], run = "cursor.bottom" },
                { on = ["x"], run = "app.quit" },
            ]
        "#;
        let eff = eff(preset, Some(user)).unwrap();
        let mut r = Resolver::new(eff.clone());
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run("cursor.top".into()),
            "prepend PISA al preset"
        );
        assert_eq!(
            r.push(parse_chord("k").unwrap()),
            Resolution::Run("cursor.up".into()),
            "append NO pisa una secuencia existente"
        );
        assert_eq!(
            r.push(parse_chord("x").unwrap()),
            Resolution::Run("app.quit".into()),
            "append añade lo nuevo"
        );
    }

    #[test]
    fn el_contexto_especifico_pisa_al_global_por_secuencia_exacta() {
        let preset = r#"
            [global]
            keymap = [
                { on = ["q"], run = "app.quit" },
                { on = ["tab"], run = "pane.switch" },
            ]
            [pane]
            keymap = [{ on = ["q"], run = "cursor.up" }]
        "#;
        let eff = eff(preset, None).unwrap();
        let mut r = Resolver::new(eff.clone());
        assert_eq!(
            r.push(parse_chord("q").unwrap()),
            Resolution::Run("cursor.up".into())
        );
        assert_eq!(
            r.push(parse_chord("tab").unwrap()),
            Resolution::Run("pane.switch".into())
        );
    }

    #[test]
    fn capas_multiples_se_pliegan_por_precedencia() {
        // Capas en precedencia ASCENDENTE: sistema, usuario.
        const COMANDOS: &[&str] = &[
            "app.quit",
            "cursor.up",
            "cursor.down",
            "cursor.top",
            "cursor.bottom",
        ];
        // ADR 0007: prepends de capas superiores primero; appends igual.
        let preset = parse_keymap(
            r#"
            [pane]
            keymap = [{ on = ["j"], run = "cursor.down" }]
        "#,
        )
        .unwrap();
        let sistema = parse_keymap(
            r#"
            [pane]
            prepend_keymap = [{ on = ["j"], run = "cursor.up" }]
            append_keymap = [{ on = ["x"], run = "app.quit" }]
        "#,
        )
        .unwrap();
        let usuario = parse_keymap(
            r#"
            [pane]
            prepend_keymap = [{ on = ["j"], run = "cursor.top" }]
            append_keymap = [{ on = ["x"], run = "cursor.bottom" }]
        "#,
        )
        .unwrap();
        let eff = Effective::build_layered(&preset, &[sistema, usuario], COMANDOS).unwrap();
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run("cursor.top".into()),
            "el prepend de la capa MÁS alta gana"
        );
        assert_eq!(
            r.push(parse_chord("x").unwrap()),
            Resolution::Run("cursor.bottom".into()),
            "entre appends también gana la capa más alta"
        );
    }

    #[test]
    fn el_contexto_viewer_se_fusiona_para_su_pantalla() {
        const COMANDOS: &[&str] = &["app.quit", "nav.enter", "cursor.top"];
        let preset = parse_keymap(
            r#"
            [global]
            keymap = [{ on = ["q"], run = "app.quit" }]
            [pane]
            keymap = [{ on = ["enter"], run = "nav.enter" }]
            [viewer]
            keymap = [{ on = ["q"], run = "cursor.top" }]
        "#,
        )
        .unwrap();
        // En Browse, el q global manda y enter existe.
        let browse = Effective::build_for(&preset, &[], COMANDOS, Screen::Browse).unwrap();
        let mut r = Resolver::new(browse);
        assert_eq!(
            r.push(parse_chord("q").unwrap()),
            Resolution::Run("app.quit".into())
        );
        assert_eq!(
            r.push(parse_chord("enter").unwrap()),
            Resolution::Run("nav.enter".into())
        );
        // En Viewer, su q específico PISA al global y enter NO existe.
        let viewer = Effective::build_for(&preset, &[], COMANDOS, Screen::Viewer).unwrap();
        let mut r = Resolver::new(viewer);
        assert_eq!(
            r.push(parse_chord("q").unwrap()),
            Resolution::Run("cursor.top".into())
        );
        assert_eq!(r.push(parse_chord("enter").unwrap()), Resolution::Reset);
    }

    /// La ayuda se construye del keymap EFECTIVO: los bindings expuestos
    /// reflejan preset + capas EN ORDEN de precedencia, y un binding
    /// sombreado aparece UNA vez con el comando que gana (lo que la tecla
    /// hace de verdad, no lo que el preset dice).
    #[test]
    fn bindings_expuestos_reflejan_las_capas() {
        const COMANDOS: &[&str] = &["cursor.down", "cursor.up", "cursor.top"];
        let preset = parse_keymap(
            r#"
            [pane]
            keymap = [
                { on = ["j"], run = "cursor.down" },
                { on = ["k"], run = "cursor.up" },
            ]
        "#,
        )
        .unwrap();
        let user = parse_keymap(
            r#"
            [pane]
            prepend_keymap = [{ on = ["j"], run = "cursor.top" }]
            append_keymap = [{ on = ["g", "g"], run = "cursor.top" }]
        "#,
        )
        .unwrap();
        let eff = Effective::build_layered(&preset, std::slice::from_ref(&user), COMANDOS).unwrap();
        let b = eff.bindings();
        // Sombreado: "j" UNA sola vez y gana el prepend del usuario.
        let jotas: Vec<_> = b.iter().filter(|(seq, _)| seq == "j").collect();
        assert_eq!(jotas.len(), 1, "binding sombreado duplicado: {b:?}");
        assert_eq!(jotas[0].1, "cursor.top", "debe ganar la capa del usuario");
        // Orden de precedencia: prepend del usuario antes que el preset.
        let pos = |wanted: &str| b.iter().position(|(seq, _)| seq == wanted).unwrap();
        assert!(pos("j") < pos("k"), "prepend antes que preset: {b:?}");
        assert!(
            b.iter()
                .any(|(seq, cmd)| seq == "g g" && *cmd == "cursor.top"),
            "el append del usuario aparece en la ayuda: {b:?}"
        );
    }

    /// ALTA (security review M4 Lua): `./.norte/keymap.toml` carga SIN trust,
    /// así que un repo hostil podría rebindear una tecla común (`j`, `enter`)
    /// a un comando `lua:` del init.lua de USUARIO (sin sandbox, sin
    /// confirmación, con cwd = el repo hostil). Los bindings `lua:`
    /// originados en la capa de PROYECTO se DESCARTAN (contados para el
    /// aviso de barra); los rebinds de proyecto a builtins siguen
    /// funcionando; el mismo binding en una capa de usuario SÍ resuelve.
    #[test]
    fn lua_de_keymap_de_proyecto_se_descarta_con_aviso() {
        const COMANDOS: &[&str] = &["cursor.down", "cursor.up"];
        let preset = parse_keymap(
            r#"
            [pane]
            keymap = [{ on = ["j"], run = "cursor.down" }]
        "#,
        )
        .unwrap();
        let capa = r#"
            [pane]
            prepend_keymap = [{ on = ["j"], run = "lua:pwn" }]
        "#;

        // Capa de PROYECTO: el binding lua: se descarta — la tecla cae al
        // builtin del preset — y queda contado para el aviso.
        let mut proyecto = parse_keymap(capa).unwrap();
        proyecto.mark_project();
        let eff = Effective::build_layered(&preset, std::slice::from_ref(&proyecto), COMANDOS)
            .expect("descartar no es error de carga");
        assert_eq!(eff.discarded_lua_bindings(), 1, "contado para el aviso");
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run("cursor.down".into()),
            "la tecla cae al builtin, jamás al lua: del proyecto"
        );

        // El MISMO binding en capa de USUARIO (sin marcar): resuelve normal.
        let usuario = parse_keymap(capa).unwrap();
        let eff =
            Effective::build_layered(&preset, std::slice::from_ref(&usuario), COMANDOS).unwrap();
        assert_eq!(eff.discarded_lua_bindings(), 0);
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(parse_chord("j").unwrap()),
            Resolution::Run("lua:pwn".into()),
            "en capa de usuario el binding lua: es legítimo"
        );

        // Rebind de proyecto a un BUILTIN: sigue funcionando (el descarte es
        // SOLO de `lua:` — config de proyecto inocua no se rompe).
        let mut proyecto = parse_keymap(
            r#"
            [pane]
            prepend_keymap = [{ on = ["x"], run = "cursor.up" }]
        "#,
        )
        .unwrap();
        proyecto.mark_project();
        let eff =
            Effective::build_layered(&preset, std::slice::from_ref(&proyecto), COMANDOS).unwrap();
        assert_eq!(eff.discarded_lua_bindings(), 0);
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(parse_chord("x").unwrap()),
            Resolution::Run("cursor.up".into())
        );
    }

    /// Nuevo (GUI-c T1): el motor NO conoce comandos concretos — valida
    /// contra la lista `known_commands` que le pasa el CALLER (cada
    /// frontend tiene su propio catálogo). Con "foo.bar" en la lista: OK;
    /// sin él, `UnknownCommand`.
    #[test]
    fn build_valida_contra_el_known_commands_dado() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["x"], run = "foo.bar" }]"#,
        )
        .unwrap();
        // Con "foo.bar" conocido: OK.
        assert!(Effective::build(&preset, None, &["foo.bar"]).is_ok());
        // Sin él: UnknownCommand (el motor NO conoce comandos concretos).
        assert!(matches!(
            Effective::build(&preset, None, &["otro.cmd"]),
            Err(KeymapError::UnknownCommand { .. })
        ));
    }

    /// Regresión GUI-c T2 review: una tecla que el FRONTEND no modela
    /// (p. ej. crossterm `BackTab`/`Media`, adaptada a `None`) debe romper
    /// cualquier secuencia multi-tecla en curso — el viejo `from_event`
    /// SIEMPRE empujaba al resolver (aunque fuera con un chord exótico que
    /// jamás casaba), lo que producía un `Miss` y limpiaba el pending. Un
    /// adaptador que devuelve `Option` y un caller que simplemente
    /// descarta el `None` deja el pending INTERNO intacto — `reset()` es
    /// el equivalente explícito al `Miss` que el adaptador ya no puede
    /// producir por sí solo.
    #[test]
    fn reset_rompe_la_secuencia_pendiente() {
        let kf = parse_keymap(
            r#"[pane]
keymap = [{ on = ["g", "g"], run = "cursor.top" }]"#,
        )
        .unwrap();
        let eff = Effective::build_for(&kf, &[], &["cursor.top"], Screen::Browse).unwrap();
        let mut r = Resolver::new(eff);
        assert!(matches!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('g'))),
            Resolution::Pending(_)
        ));
        r.reset();
        // Tras reset, un solo 'g' vuelve a estar pendiente (la secuencia
        // se rompió: si NO se hubiera roto, este segundo 'g' dispararía
        // Run("cursor.top") en vez de Pending(1)).
        assert!(matches!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('g'))),
            Resolution::Pending(_)
        ));
    }
}
