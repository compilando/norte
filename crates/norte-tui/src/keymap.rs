//! Keymap engine (ADR 0006): mapa `(contexto, secuencia) → comando`,
//! capas estilo Yazi (`prepend_keymap`/`append_keymap`), keymap efectivo
//! precomputado y PREFIX-FREE validado al cargar — la resolución es un
//! scan lineal determinista sobre el efectivo (≤ centenas de bindings),
//! sin timeouts.

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyModifiers};
use serde::Deserialize;

/// Una tecla con modificadores, en forma canónica.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    mods: KeyModifiers,
    code: KeyCode,
}

impl Chord {
    /// Chord canónico (para tests y construcción directa).
    #[must_use]
    pub fn new(mods: KeyModifiers, code: KeyCode) -> Self {
        Self { mods, code }
    }

    /// Chord canónico desde un evento de crossterm: en teclas `Char` el
    /// char YA codifica shift (`G`), así que el modificador se descarta;
    /// en el resto (`shift+f5`) shift es información.
    #[must_use]
    pub fn from_event(mods: KeyModifiers, code: KeyCode) -> Self {
        let mods = if matches!(code, KeyCode::Char(_)) {
            mods - KeyModifiers::SHIFT
        } else {
            mods
        };
        Self { mods, code }
    }

    fn is_bare_esc(self) -> bool {
        self.code == KeyCode::Esc && self.mods.is_empty()
    }
}

impl std::fmt::Display for Chord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.mods.contains(KeyModifiers::CONTROL) {
            f.write_str("ctrl+")?;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            f.write_str("alt+")?;
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
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
            other => write!(f, "{other:?}"),
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
    let mut mods = KeyModifiers::NONE;
    for m in mods_txt {
        let flag = match *m {
            "ctrl" => KeyModifiers::CONTROL,
            "alt" => KeyModifiers::ALT,
            "shift" => KeyModifiers::SHIFT,
            _ => return Err(bad()),
        };
        if mods.contains(flag) {
            return Err(bad());
        }
        mods |= flag;
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
    if mods.contains(KeyModifiers::SHIFT) && matches!(code, KeyCode::Char(_)) {
        return Err(KeymapError::ShiftWithChar {
            chord: s.to_owned(),
        });
    }
    Ok(Chord::new(mods, code))
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

/// Parsea un `keymap.toml`.
///
/// # Errors
/// [`KeymapError::Toml`] si no parsea o hay claves desconocidas.
pub fn parse_keymap(s: &str) -> Result<KeymapFile, KeymapError> {
    toml::from_str(s).map_err(|e| KeymapError::Toml(crate::config::toml_diag(s, &e)))
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
    /// (seguridad, ver [`KeymapFile::mark_project`]). El caller lo pinta
    /// una vez por barra (`msg-lua-keymap-project`) — jamás descarte mudo.
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
            // `valid_name` de `norte.command`, una sola fuente). Un comando
            // lua no registrado al invocar NO es error de keymap: la barra
            // avisa con `err-lua-unknown`.
            if let Some(lua_name) = raw.run.strip_prefix("lua:") {
                if !crate::lua::valid_name(lua_name) {
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

/// Los comandos que el TUI sabe ejecutar — la fuente ÚNICA contra la que
/// se valida todo keymap (los mismos nombres que verán la palette y el
/// wire, ADR 0006).
pub const COMMANDS: &[&str] = &[
    "app.quit",
    "pane.switch",
    "cursor.up",
    "cursor.down",
    "cursor.page-up",
    "cursor.page-down",
    "cursor.top",
    "cursor.bottom",
    "nav.enter",
    "nav.parent",
    "app.help",
    "app.theme",
    "app.extensions",
    "pane.copy",
    "pane.move",
    "pane.delete",
    "pane.delete-permanent",
    "pane.view",
    "task.cancel",
    "viewer.close",
    "viewer.up",
    "viewer.down",
    "viewer.page-up",
    "viewer.page-down",
    "viewer.top",
    "viewer.bottom",
    "viewer.encoding",
    "viewer.encoding-auto",
    "viewer.hex",
];

/// Id de Fluent con la descripción de un comando (`app.quit` →
/// `help-cmd-app-quit`). La suite OBLIGA a que exista en ambos locales
/// para TODO comando de [`COMMANDS`]: un comando nuevo sin descripción
/// rompe tests — la ayuda no puede quedarse atrás.
#[must_use]
pub fn help_id(command: &str) -> String {
    format!("help-cmd-{}", command.replace('.', "-"))
}

/// Los presets de fábrica, parseados (se validan en tests y al construir
/// el efectivo). Default del producto: `orthodox` (decisión 2026-07-10).
///
/// # Panics
/// Nunca con los TOML embebidos (los valida la suite).
#[must_use]
pub fn presets() -> Vec<(&'static str, KeymapFile)> {
    [
        ("orthodox", include_str!("keymap_presets/orthodox.toml")),
        ("vim", include_str!("keymap_presets/vim.toml")),
        ("cua", include_str!("keymap_presets/cua.toml")),
    ]
    .into_iter()
    .map(|(name, src)| {
        (
            name,
            parse_keymap(src).unwrap_or_else(|e| panic!("preset {name} embebido inválido: {e}")),
        )
    })
    .collect()
}
