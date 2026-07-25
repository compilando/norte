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

/// One finding from [`Effective::build_diagnostics`]: reported WITHOUT
/// stopping the walk, unlike [`Effective::build_for`], which fails on the
/// first defect. `norte doctor` (#102) maps each to a report row in a
/// SINGLE pass — no per-typo rebuild, no retry cap, and no non-convergent
/// `lua:`-charset case (a broken `lua:` name can never be fixed by extending
/// `known_commands`, so the retry-with-known trick never terminates for it;
/// the one-pass walk classifies it directly instead).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeymapDiagnostic {
    /// A plain (non-`lua:`) `run` name absent from `known_commands` — a typo
    /// or a binding for a newer version's command. Recoverable: the rest of
    /// the keymap is unaffected. `run` is UNTRUSTED config text.
    UnknownCommand {
        /// The unrecognized command name (untrusted config text).
        run: String,
    },
    /// A defect that would make [`Effective::build_for`] fail outright: a bad
    /// chord, an empty or `esc`-bearing sequence, a wrong layer key, an
    /// ambiguous prefix, or a `lua:` name that fails the charset. Carries the
    /// rendered [`KeymapError`] message (may embed UNTRUSTED config text).
    Structural {
        /// Human-readable description (from the underlying [`KeymapError`]).
        message: String,
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

/// One binding as represented in TOML.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct RawBinding {
    on: Vec<String>,
    run: String,
}

/// The three binding lists in a section: `keymap` for presets and
/// `prepend_keymap`/`append_keymap` for user layers.
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

/// A parsed `keymap.toml` preset or user layer.
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
    /// Contexto `dialog` (H1, issue #24): teclas de modales/overlays
    /// (confirmación, aprobación, popups de navegación…) como keymap de
    /// datos en vez de handlers ad hoc — la ayuda generada nunca puede
    /// desincronizarse de un rebind. Se fusiona con `global` igual que
    /// `pane`/`viewer` (ver [`Screen::Dialog`]).
    #[serde(default)]
    dialog: RawSection,
    /// `true` si esta capa es la de PROYECTO (`./.norte`) — contenido
    /// potencialmente AJENO (viene con un repo clonado) que se carga SIN
    /// trust. Un keymap de proyecto NO puede bindear `lua:`:
    /// [`Effective::build_for`] descarta esos bindings (contados en
    /// [`Effective::discarded_lua_bindings`]) — rebindear una tecla común a
    /// un comando del `init.lua` del USUARIO (sin sandbox) sería ejecución
    /// dirigida por el repo sin confirmación alguna. No viene del TOML
    /// (`serde(skip)`): lo marca `load_keymap_layer` (`config.rs`) leyendo
    /// el [`Layer`](norte_config::Layer) del `dir` que trae cada capa
    /// (ADR 0035: el kind viaja POR DIR en `Layers`, ya no se infiere por
    /// posición — deuda #75 cerrada).
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
            || !self.dialog.keymap.is_empty()
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
/// `global` (ADR 0006; el stack crece con la UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Los dos panes (contexto `pane`).
    Browse,
    /// El viewer (contexto `viewer`, fase 7).
    Viewer,
    /// Modales/overlays (contexto `dialog`, H1 — issue #24): confirmación,
    /// aprobación, popups de navegación… cada overlay declara su propio
    /// ALLOWLIST de qué `dialog.*` comandos soporta (la semántica de
    /// seguridad vive en código, no aquí).
    Dialog,
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

/// Where a merged binding comes from: the shared preset (eligible for the
/// lenient filter in [`Effective::build_for_subset`]) or a user/project
/// layer (always strict — see [`Strictness`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Preset,
    Layer,
}

/// Selects the behavior for a PRESET binding to a command absent from
/// `known_commands`: [`Effective::build_for`] is `Strict` (error),
/// [`Effective::build_for_subset`] is `Lenient` (silently filtered — the
/// frontend implements a subset of the shared preset's commands). Never
/// affects layer bindings — see the `continue` in `build_for_impl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strictness {
    Strict,
    Lenient,
}

/// Fusión de un contexto (ADR 0006/0007): prepends de capa superior primero
/// (ganan), luego el preset, luego los appends (superiores antes). Los
/// bindings `lua:` de una capa de PROYECTO se DESCARTAN aquí, contados en
/// `discarded_lua` (seguridad: ver [`KeymapFile::mark_project`] — el
/// keymap de un repo ajeno no puede dirigir la ejecución de comandos Lua).
/// Cada binding se etiqueta con su [`Origin`] (preset vs. capa) para que
/// `build_for_impl` sepa a cuáles aplica el filtrado lenient.
fn merge_ctx<'a>(
    preset: &'a KeymapFile,
    layers: &'a [KeymapFile],
    get: fn(&KeymapFile) -> &RawSection,
    discarded_lua: &mut usize,
) -> Vec<(&'a RawBinding, Origin)> {
    let mut out = Vec::new();
    let mut push =
        |layer_project: bool, b: &'a RawBinding, out: &mut Vec<(&'a RawBinding, Origin)>| {
            if layer_project && b.run.starts_with("lua:") {
                *discarded_lua += 1;
            } else {
                out.push((b, Origin::Layer));
            }
        };
    for l in layers.iter().rev() {
        for b in &get(l).prepend_keymap {
            push(l.project, b, &mut out);
        }
    }
    out.extend(get(preset).keymap.iter().map(|b| (b, Origin::Preset)));
    for l in layers.iter().rev() {
        for b in &get(l).append_keymap {
            push(l.project, b, &mut out);
        }
    }
    out
}

/// Charset de un nombre de comando Lua (`lua:<nombre>`): `[a-z0-9._-]{1,64}`.
/// FUENTE ÚNICA (#88): el motor lo usa para validar el binding `lua:<nombre>`,
/// y el runtime Lua de un frontend con host (la TUI, `norte.command`) lo reusa
/// para validar el nombre registrado — así el charset no puede derivar entre
/// «lo que el keymap acepta» y «lo que el runtime registra».
#[must_use]
pub fn valid_lua_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// Each layer admits ONLY its own lists (phase-4 review): a preset defines
/// `keymap`; a user/project layer defines `prepend_keymap`/`append_keymap`.
/// Silently dropping the wrong list would be the "weird behavior" the ADR
/// forbids. Returns the first offending layer/key (there is at most one kind
/// of mistake worth reporting per source). Shared by [`Effective::build_for`]
/// (fails on it) and [`Effective::build_diagnostics`] (reports it and keeps
/// walking — the bindings still merge from the CORRECT lists via `merge_ctx`).
fn check_layer_keys(preset: &KeymapFile, layers: &[KeymapFile]) -> Result<(), KeymapError> {
    for section in [&preset.global, &preset.pane, &preset.viewer, &preset.dialog] {
        if !(section.prepend_keymap.is_empty() && section.append_keymap.is_empty()) {
            return Err(KeymapError::WrongLayerKey {
                layer: "preset",
                key: "prepend_keymap/append_keymap",
            });
        }
    }
    for layer in layers {
        for section in [&layer.global, &layer.pane, &layer.viewer, &layer.dialog] {
            if !section.keymap.is_empty() {
                return Err(KeymapError::WrongLayerKey {
                    layer: "usuario",
                    key: "keymap",
                });
            }
        }
    }
    Ok(())
}

/// Validates ONE merged binding. `Ok(Some(binding))` = keep it;
/// `Ok(None)` = a lenient-filtered PRESET binding (a command this frontend
/// does not implement — skipped silently, only in [`Strictness::Lenient`]);
/// `Err` = a defect. Shared by [`Effective::build_for`] (fails on the first
/// `Err`) and [`Effective::build_diagnostics`] (collects every `Err` and
/// keeps walking) — the SINGLE source of the per-binding rules, so the two
/// paths can never drift.
fn check_binding(
    raw: &RawBinding,
    origin: Origin,
    known_commands: &[&str],
    preset_strictness: Strictness,
) -> Result<Option<(Vec<Chord>, String)>, KeymapError> {
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
    // Esc es la cancelación de secuencia (lo cazó el proptest: un esc
    // no-inicial sería inalcanzable): solo como binding suelto.
    if seq.len() > 1 && seq.iter().any(|c| c.is_bare_esc()) {
        return Err(KeymapError::EscInSequence {
            sequence: format!("{:?}", raw.on),
        });
    }
    // `lua:<nombre>` (M4 Lua, T8): el registro de comandos Lua es DINÁMICO
    // (runtime), así que no se valida contra `known_commands` — solo el
    // charset del nombre (la MISMA `valid_lua_name`, una sola fuente). Un
    // comando lua no registrado al invocar NO es error de keymap: el frontend
    // con host avisa en runtime. Se aplica IGUAL en modo lenient — el
    // filtrado de `build_for_subset` es SOLO por `known_commands` desconocido,
    // jamás una vía para colarse del charset lua:.
    if let Some(lua_name) = raw.run.strip_prefix("lua:") {
        if !valid_lua_name(lua_name) {
            return Err(KeymapError::UnknownCommand {
                run: raw.run.clone(),
            });
        }
    } else if !known_commands.contains(&raw.run.as_str()) {
        // En modo Lenient, SOLO los bindings del `keymap` del preset se
        // filtran en silencio (el frontend no implementa ese comando
        // compartido); un binding de CAPA (prepend/append de usuario o
        // proyecto) sigue siendo estricto — un typo de usuario jamás debe
        // morir en silencio (ADR 0006).
        if preset_strictness == Strictness::Lenient && origin == Origin::Preset {
            return Ok(None);
        }
        return Err(KeymapError::UnknownCommand {
            run: raw.run.clone(),
        });
    }
    Ok(Some((seq, raw.run.clone())))
}

/// Prefix-free: no sequence is a strict prefix of another (ADR 0006 — without
/// timeouts, resolution must be deterministic). Runs over the ALREADY-filtered
/// set — a preset binding skipped in `Lenient` mode must not block a foreign
/// prefix. Returns the first ambiguous pair; shared by both builders.
fn check_prefix_free(bindings: &[(Vec<Chord>, String)]) -> Result<(), KeymapError> {
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
    Ok(())
}

/// The merged, ordered binding list for `screen` (screen-specific context
/// before `global`, ADR 0006), with project-layer `lua:` bindings discarded
/// and counted. Shared by [`Effective::build_for`] and
/// [`Effective::build_diagnostics`] so the merge order is defined once.
fn merged_bindings<'a>(
    preset: &'a KeymapFile,
    layers: &'a [KeymapFile],
    screen: Screen,
    discarded_lua_bindings: &mut usize,
) -> Vec<(&'a RawBinding, Origin)> {
    let specific: fn(&KeymapFile) -> &RawSection = match screen {
        Screen::Browse => |f| &f.pane,
        Screen::Viewer => |f| &f.viewer,
        Screen::Dialog => |f| &f.dialog,
    };
    merge_ctx(preset, layers, specific, discarded_lua_bindings)
        .into_iter()
        .chain(merge_ctx(
            preset,
            layers,
            |f| &f.global,
            discarded_lua_bindings,
        ))
        .collect()
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
        Self::build_for_impl(preset, layers, known_commands, screen, Strictness::Strict)
    }

    /// Like [`Effective::build_for`], but PRESET bindings whose command is
    /// not in `known_commands` are skipped instead of failing. For
    /// frontends that implement a subset of the shared presets' commands
    /// (the GUI). User/project LAYERS remain strict.
    ///
    /// # Errors
    /// Same as [`Effective::build_for`], except preset bindings to
    /// non-`lua:` commands absent from `known_commands` are skipped
    /// instead of failing.
    pub fn build_for_subset(
        preset: &KeymapFile,
        layers: &[KeymapFile],
        known_commands: &[&str],
        screen: Screen,
    ) -> Result<Self, KeymapError> {
        Self::build_for_impl(preset, layers, known_commands, screen, Strictness::Lenient)
    }

    fn build_for_impl(
        preset: &KeymapFile,
        layers: &[KeymapFile],
        known_commands: &[&str],
        screen: Screen,
        preset_strictness: Strictness,
    ) -> Result<Self, KeymapError> {
        check_layer_keys(preset, layers)?;
        let mut discarded_lua_bindings = 0usize;
        let ordered = merged_bindings(preset, layers, screen, &mut discarded_lua_bindings);

        let mut seen: HashSet<Vec<Chord>> = HashSet::new();
        let mut bindings: Vec<(Vec<Chord>, String)> = Vec::new();
        for (raw, origin) in ordered {
            if let Some((seq, run)) = check_binding(raw, origin, known_commands, preset_strictness)?
            {
                // El primero gana (el orden YA codifica la precedencia).
                if seen.insert(seq.clone()) {
                    bindings.push((seq, run));
                }
            }
        }
        check_prefix_free(&bindings)?;
        Ok(Self {
            bindings,
            discarded_lua_bindings,
        })
    }

    /// Builds the effective keymap for `screen` in a SINGLE pass, collecting
    /// EVERY defect as a [`KeymapDiagnostic`] instead of failing on the first
    /// (as [`Effective::build_for`] does). Unlike the diagnostic loop it
    /// replaces (issue #102), it needs no per-typo rebuild and no anti-DoS
    /// retry cap, and it cannot get stuck on the non-convergent `lua:`-charset
    /// case: a broken `lua:` name is classified directly as
    /// [`KeymapDiagnostic::Structural`] (extending `known_commands` could never
    /// fix it). Strictness matches [`Effective::build_for`] (`Strict`) — the
    /// caller (`norte doctor`) validates against the union of the bundled
    /// presets' commands, so a preset binding is never silently filtered here.
    ///
    /// Findings appear in walk order: wrong-layer-key (if any), then each
    /// binding's defect, then the first ambiguous prefix. An empty result
    /// means the keymap is clean.
    #[must_use]
    pub fn build_diagnostics(
        preset: &KeymapFile,
        layers: &[KeymapFile],
        known_commands: &[&str],
        screen: Screen,
    ) -> Vec<KeymapDiagnostic> {
        let mut diags = Vec::new();
        if let Err(e) = check_layer_keys(preset, layers) {
            // A wrong layer key does not stop the walk: `merge_ctx` reads the
            // CORRECT lists, so binding-level findings are still worth
            // reporting in the same pass.
            diags.push(KeymapDiagnostic::Structural {
                message: e.to_string(),
            });
        }
        let mut discarded = 0usize;
        let ordered = merged_bindings(preset, layers, screen, &mut discarded);
        let mut seen: HashSet<Vec<Chord>> = HashSet::new();
        let mut bindings: Vec<(Vec<Chord>, String)> = Vec::new();
        for (raw, origin) in ordered {
            match check_binding(raw, origin, known_commands, Strictness::Strict) {
                Ok(Some((seq, run))) => {
                    if seen.insert(seq.clone()) {
                        bindings.push((seq, run));
                    }
                }
                // Unreachable under `Strict` (no lenient filtering), but a
                // filtered binding is simply skipped either way.
                Ok(None) => {}
                Err(KeymapError::UnknownCommand { run })
                    if run.strip_prefix("lua:").is_none_or(valid_lua_name) =>
                {
                    // A PLAIN unknown command (or a well-formed `lua:` name
                    // that just is not in `known` — impossible here since
                    // valid `lua:` names are accepted, so this arm is the
                    // plain case): recoverable.
                    diags.push(KeymapDiagnostic::UnknownCommand { run });
                }
                // The remaining `UnknownCommand` is a `lua:` name that FAILS
                // the charset (`valid_lua_name` — single source): structural,
                // never fixable by extending `known_commands`.
                Err(e) => diags.push(KeymapDiagnostic::Structural {
                    message: e.to_string(),
                }),
            }
        }
        if let Err(e) = check_prefix_free(&bindings) {
            diags.push(KeymapDiagnostic::Structural {
                message: e.to_string(),
            });
        }
        diags
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

    /// `+` is the modifier separator, so a bare "+" is unparseable and `plus`
    /// is the only spelling. Pinned because a future refactor that "simplifies"
    /// the token table would silently make the mark.pattern-add chord
    /// unreachable (#103).
    #[test]
    fn plus_token_is_the_only_spelling_of_the_plus_key() {
        assert_eq!(
            parse_chord("plus").unwrap(),
            Chord::new(Mods::default(), KeyCode::Char('+'))
        );
        assert!(matches!(
            parse_chord("+"),
            Err(KeymapError::BadChord { .. })
        ));
        assert_eq!(
            parse_chord("ctrl+plus").unwrap(),
            Chord::new(
                Mods {
                    ctrl: true,
                    ..Default::default()
                },
                KeyCode::Char('+')
            )
        );
    }

    #[test]
    fn plus_chord_round_trips_through_display() {
        let c = Chord::new(Mods::default(), KeyCode::Char('+'));
        assert_eq!(c.to_string(), "plus");
        assert_eq!(parse_chord(&c.to_string()).unwrap(), c);
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
    fn parse_chord_rechaza_tokens_multi_codepoint_sin_partir() {
        // Un token que NO es exactamente un char (é descompuesto = e+U+0301,
        // o un emoji ZWJ) se rechaza limpio, jamás se trunca a medias.
        assert!(matches!(
            parse_chord("e\u{0301}"),
            Err(KeymapError::BadChord { .. })
        ));
        assert!(matches!(
            parse_chord("👨\u{200d}👩\u{200d}👧"),
            Err(KeymapError::BadChord { .. })
        ));
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

    /// `build_for_subset`: a PRESET binding to a command this frontend does
    /// not implement is skipped (the GUI implements a subset of the TUI's
    /// commands); a LAYER binding to an unknown command is still an error
    /// (a user typo must never die silently — ADR 0006).
    #[test]
    fn build_for_subset_filtra_preset_pero_capa_sigue_estricta() {
        let preset = parse_keymap(
            "[pane]\nkeymap = [\n { on = [\"q\"], run = \"app.quit\" },\n { on = [\"f1\"], run = \"app.help\" },\n]\n",
        )
        .unwrap();
        let known = ["app.quit"];
        let eff = Effective::build_for_subset(&preset, &[], &known, Screen::Browse)
            .expect("preset con extras construye");
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('q'))),
            Resolution::Run("app.quit".into())
        );
        // El binding filtrado no existe: F1 no tiene ningún binding —
        // Miss, no Prefix ni Run.
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::F(1))),
            Resolution::Reset
        );
        let layer =
            parse_keymap("[pane]\nprepend_keymap = [{ on = [\"z\"], run = \"app.help\" }]\n")
                .unwrap();
        assert!(
            Effective::build_for_subset(&preset, &[layer], &known, Screen::Browse).is_err(),
            "capa con comando desconocido: error, no filtrado"
        );
    }

    /// El nombre `lua:` sigue validándose por CHARSET aunque el modo sea
    /// Lenient — el filtrado de `build_for_subset` es solo por
    /// `known_commands` ausente; un `lua:` con nombre inválido (fuera de
    /// `[a-z0-9._-]{1,64}`) no tiene forma de colarse. El error sale como
    /// `UnknownCommand` (mismo camino que en modo estricto: el check de
    /// charset vive ANTES del filtrado lenient).
    #[test]
    fn subset_lua_invalido_sigue_siendo_error() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["x"], run = "lua:Bad Name" }]"#,
        )
        .unwrap();
        match Effective::build_for_subset(&preset, &[], &[], Screen::Browse) {
            Err(KeymapError::UnknownCommand { .. }) => {}
            other => panic!("esperaba UnknownCommand, fue {other:?}"),
        }
    }

    /// `build_diagnostics` (#102): reports EVERY unknown-command finding in
    /// ONE walk — no per-typo rebuild, no retry cap. Three distinct made-up
    /// `run` names in a layer must all come back as `UnknownCommand`
    /// diagnostics from a single call.
    #[test]
    fn build_diagnostics_reporta_todos_los_desconocidos_en_una_pasada() {
        let preset =
            parse_keymap("[pane]\nkeymap = [{ on = [\"q\"], run = \"app.quit\" }]\n").unwrap();
        let layer = parse_keymap(
            "[pane]\nappend_keymap = [\n { on = [\"x\"], run = \"typo.one\" },\n { on = [\"y\"], run = \"typo.two\" },\n { on = [\"z\"], run = \"typo.three\" },\n]\n",
        )
        .unwrap();
        let known = ["app.quit"];
        let diags = Effective::build_diagnostics(&preset, &[layer], &known, Screen::Browse);
        let unknowns: Vec<&str> = diags
            .iter()
            .filter_map(|d| match d {
                KeymapDiagnostic::UnknownCommand { run } => Some(run.as_str()),
                KeymapDiagnostic::Structural { .. } => None,
            })
            .collect();
        assert_eq!(
            unknowns,
            ["typo.one", "typo.two", "typo.three"],
            "{diags:?}"
        );
    }

    /// A `lua:<name>` binding whose name fails the charset is a `Structural`
    /// diagnostic (never fixable by adding it to `known`), NOT a recoverable
    /// `UnknownCommand` — this is exactly the non-convergent case #102's
    /// one-pass builder resolves by construction.
    #[test]
    fn build_diagnostics_lua_charset_invalido_es_structural() {
        let preset =
            parse_keymap("[pane]\nkeymap = [{ on = [\"x\"], run = \"lua:bad name!\" }]\n").unwrap();
        let diags = Effective::build_diagnostics(&preset, &[], &[], Screen::Browse);
        assert_eq!(diags.len(), 1, "{diags:?}");
        match &diags[0] {
            KeymapDiagnostic::Structural { message } => {
                assert!(message.contains("lua:bad name!"), "{message}");
            }
            d @ KeymapDiagnostic::UnknownCommand { .. } => {
                panic!("esperaba Structural, fue {d:?}")
            }
        }
    }

    /// A well-formed keymap yields NO diagnostics (the caller reports
    /// `keymap-ok`).
    #[test]
    fn build_diagnostics_keymap_valido_sin_hallazgos() {
        let preset =
            parse_keymap("[pane]\nkeymap = [{ on = [\"q\"], run = \"app.quit\" }]\n").unwrap();
        let diags = Effective::build_diagnostics(&preset, &[], &["app.quit"], Screen::Browse);
        assert!(diags.is_empty(), "{diags:?}");
    }

    /// Un binding de PRESET filtrado (comando desconocido para este
    /// frontend) no puede bloquear una secuencia más larga que lo tenía
    /// como prefijo — pin de la afirmación "prefix-freeness corre sobre el
    /// set YA filtrado". El binding largo viene de una CAPA (no del
    /// preset): en modo estricto, "g" (preset) + "g g" (capa) sería
    /// `AmbiguousPrefix`; en Lenient, "g" se filtra antes del check y "g g"
    /// resuelve limpio.
    #[test]
    fn subset_prefijo_filtrado_no_bloquea_secuencia() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["g"], run = "gui.unknown" }]"#,
        )
        .unwrap();
        let layer = parse_keymap(
            r#"[pane]
append_keymap = [{ on = ["g", "g"], run = "known.cmd" }]"#,
        )
        .unwrap();
        let known = ["known.cmd"];
        let eff = Effective::build_for_subset(&preset, &[layer], &known, Screen::Browse)
            .expect("el prefijo filtrado no debe producir AmbiguousPrefix");
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('g'))),
            Resolution::Pending(1),
            "único binding activo en g: prefijo válido de g g"
        );
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('g'))),
            Resolution::Run("known.cmd".into())
        );
    }

    /// Divergencia DELIBERADA entre Strict y Lenient, pineada a propósito:
    /// el dedup por secuencia (`seen.insert`, "el primero gana") solo ve
    /// los bindings que SOBREVIVEN al filtro. Con un binding de `pane` y
    /// otro de `global` en el MISMO chord, un frontend que conoce ambos
    /// comandos (Strict) ve ganar `pane` por especificidad de contexto —
    /// pero un frontend que NO implementa el comando de `pane` (Lenient) lo
    /// filtra ANTES del dedup, y el binding de `global` queda "desenmascarado"
    /// (deja de estar sombreado) y pasa a ser el activo. Es el precio de
    /// que cada frontend valide contra SU PROPIO catálogo: la tecla hace
    /// algo distinto según qué frontend la interprete, por diseño (ADR
    /// 0006 — el motor no conoce comandos concretos).
    #[test]
    fn subset_dedup_desenmascara_binding_global() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["x"], run = "gui.unknown" }]
[global]
keymap = [{ on = ["x"], run = "app.quit" }]"#,
        )
        .unwrap();
        let known = ["app.quit"];
        let eff = Effective::build_for_subset(&preset, &[], &known, Screen::Browse)
            .expect("pane.x filtrado, global.x conocido");
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('x'))),
            Resolution::Run("app.quit".into()),
            "con pane.x filtrado, global.x deja de estar sombreado"
        );
    }

    /// Un chord ilegible (`"megatecla"`) en un binding de PRESET cuyo
    /// comando TAMBIÉN es desconocido: el parseo de la secuencia corre
    /// ANTES del filtrado lenient (`raw.on.iter().map(parse_chord)`), así
    /// que `BadChord` gana incluso en modo Lenient — el filtro solo
    /// silencia comandos desconocidos, jamás config estructuralmente rota.
    #[test]
    fn subset_chord_malo_en_preset_sigue_fallando() {
        let preset = parse_keymap(
            r#"[pane]
keymap = [{ on = ["megatecla"], run = "gui.unknown" }]"#,
        )
        .unwrap();
        match Effective::build_for_subset(&preset, &[], &[], Screen::Browse) {
            Err(KeymapError::BadChord { .. }) => {}
            other => panic!("esperaba BadChord, fue {other:?}"),
        }
    }

    /// H1 (#24): el contexto `dialog` existe — un preset con [dialog]
    /// construye y resuelve para `Screen::Dialog`.
    #[test]
    fn dialog_context_se_parsea_y_construye() {
        let preset =
            parse_keymap("[dialog]\nkeymap = [{ on = [\"y\"], run = \"dialog.approve\" }]\n")
                .unwrap();
        let eff = Effective::build_for(&preset, &[], &["dialog.approve"], Screen::Dialog)
            .expect("construye");
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('y'))),
            Resolution::Run("dialog.approve".into())
        );
    }

    /// Una capa de usuario extiende [dialog] con prepend y GANA.
    #[test]
    fn capa_puede_extender_dialog() {
        let preset =
            parse_keymap("[dialog]\nkeymap = [{ on = [\"y\"], run = \"dialog.approve\" }]\n")
                .unwrap();
        let layer =
            parse_keymap("[dialog]\nprepend_keymap = [{ on = [\"y\"], run = \"dialog.deny\" }]\n")
                .unwrap();
        let eff = Effective::build_for(
            &preset,
            &[layer],
            &["dialog.approve", "dialog.deny"],
            Screen::Dialog,
        )
        .expect("construye");
        let mut r = Resolver::new(eff);
        assert_eq!(
            r.push(Chord::new(Mods::default(), KeyCode::Char('y'))),
            Resolution::Run("dialog.deny".into())
        );
    }

    /// Capa con `keymap` completo en [dialog]: error, como en el resto.
    #[test]
    fn has_full_keymap_ve_dialog() {
        let layer =
            parse_keymap("[dialog]\nkeymap = [{ on = [\"y\"], run = \"dialog.approve\" }]\n")
                .unwrap();
        assert!(layer.has_full_keymap());
    }
}

/// Bundled keymap presets (ADR 0006), shared by every frontend. Each
/// frontend validates against ITS OWN command set — via
/// [`Effective::build_for`] (strict) or [`Effective::build_for_subset`]
/// (preset bindings to commands the frontend lacks are skipped).
pub mod presets {
    /// The default orthodox preset.
    pub const ORTHODOX: &str = include_str!("../presets/keymap/orthodox.toml");
    /// Vim-style preset.
    pub const VIM: &str = include_str!("../presets/keymap/vim.toml");
    /// CUA preset.
    pub const CUA: &str = include_str!("../presets/keymap/cua.toml");

    /// Names of every embedded preset (final review MINOR 4: this catalog
    /// used to be mirrored as a hardcoded `&[&str]` in each frontend that
    /// needs a "known preset" list or an "available presets" banner message
    /// — TUI, GUI — with no single source of truth. `source()` and `NAMES`
    /// are now tested against each other below, so a preset added to one and
    /// not the other fails CI instead of drifting silently.
    pub const NAMES: &[&str] = &["orthodox", "vim", "cua"];

    /// Preset source by name; `None` if unknown (caller falls back +
    /// reports, same contract the TUI had).
    #[must_use]
    pub fn source(name: &str) -> Option<&'static str> {
        match name {
            "orthodox" => Some(ORTHODOX),
            "vim" => Some(VIM),
            "cua" => Some(CUA),
            _ => None,
        }
    }
}

/// The union of `run` names bound (in ANY of the three lists — `keymap`,
/// `prepend_keymap`, `append_keymap`, though only `keymap` is actually used
/// by a bundled preset today) by ANY bundled preset (`orthodox`/`vim`/`cua`)
/// for `screen` — its screen-specific context (`pane`/`viewer`/`dialog`)
/// merged with `global`. A preset that fails to parse is skipped silently
/// (the three bundled presets are compile-time embedded and pinned by this
/// module's own test suite, so this only matters if that invariant ever
/// breaks).
///
/// This is an HONEST APPROXIMATION, not a frontend's actual command
/// catalog: it only sees command names bound by a keymap, not the full set
/// a frontend implements (a command with no default binding in any preset
/// is invisible here). `norte doctor` (H2) uses it to flag a config layer's
/// `run` name that no bundled preset recognizes for that screen — worth a
/// warning, not proof the command doesn't exist (see its rustdoc/report
/// footer for the caveat).
#[must_use]
pub fn preset_commands(screen: Screen) -> Vec<String> {
    let specific: fn(&KeymapFile) -> &RawSection = match screen {
        Screen::Browse => |f| &f.pane,
        Screen::Viewer => |f| &f.viewer,
        Screen::Dialog => |f| &f.dialog,
    };
    let mut out: Vec<String> = Vec::new();
    let push_all = |section: &RawSection, out: &mut Vec<String>| {
        for list in [
            &section.keymap,
            &section.prepend_keymap,
            &section.append_keymap,
        ] {
            for b in list {
                if !out.contains(&b.run) {
                    out.push(b.run.clone());
                }
            }
        }
    };
    for name in presets::NAMES {
        let Some(src) = presets::source(name) else {
            continue;
        };
        let Ok(kf) = parse_keymap(src) else {
            continue;
        };
        push_all(specific(&kf), &mut out);
        push_all(&kf.global, &mut out);
    }
    out
}

#[cfg(test)]
mod preset_commands_tests {
    use super::{Screen, preset_commands};

    /// Orthodox binds `app.quit` in `[global]` (ADR 0006: global merges
    /// into every screen), so `Browse` must see it.
    #[test]
    fn orthodox_browse_contiene_app_quit() {
        let v = preset_commands(Screen::Browse);
        assert!(v.contains(&"app.quit".to_owned()), "{v:?}");
    }

    /// Every bundled preset binds `y` to `dialog.approve` in `[dialog]`.
    #[test]
    fn dialog_contiene_dialog_approve() {
        let v = preset_commands(Screen::Dialog);
        assert!(v.contains(&"dialog.approve".to_owned()), "{v:?}");
    }
}

#[cfg(test)]
mod presets_catalog_tests {
    use super::presets::{NAMES, source};

    /// `NAMES` es el catálogo — cada entrada debe resolver una fuente real
    /// (final review MINOR 4: sin esto, `NAMES` podría desincronizarse de
    /// `source()` sin que ningún test lo note).
    #[test]
    fn cada_nombre_de_names_resuelve_una_fuente() {
        for name in NAMES {
            assert!(
                source(name).is_some(),
                "NAMES declara {name:?} pero source({name:?}) es None"
            );
        }
    }

    /// Ancla el tamaño del catálogo: un preset nuevo debe tocar este test a
    /// propósito (y con él, el resto de frontends que consumen `NAMES`).
    #[test]
    fn names_tiene_los_tres_presets_de_fabrica() {
        assert_eq!(NAMES.len(), 3);
    }
}
